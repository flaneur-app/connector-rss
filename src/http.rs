// SPDX-License-Identifier: AGPL-3.0-or-later

//! The one HTTP client of the connector: polite User-Agent, timeouts, an in-memory cache of
//! documents revalidated with `ETag` / `Last-Modified`, and ranged probes of media URLs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use flaneur_connector::{Error, Result};
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode};
use url::Url;

use crate::media::{self, MediaClass};

/// `User-Agent` sent with every request. It names the client and its site, nothing about the
/// user.
pub const USER_AGENT: &str = concat!(
    "Flaneur/",
    env!("CARGO_PKG_VERSION"),
    " (+https://flaneur.baxyz.dev)"
);

const ACCEPT_FEED: &str = "application/rss+xml, application/atom+xml, application/feed+json, \
                           application/xml;q=0.9, text/xml;q=0.9, */*;q=0.5";
const ACCEPT_PAGE: &str = "text/html, application/xhtml+xml;q=0.9, */*;q=0.5";

/// Tunables of the HTTP client.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Total time allowed for one request.
    pub timeout: Duration,
    /// Time allowed to connect.
    pub connect_timeout: Duration,
    /// A cached document younger than this is reused without asking the server again. Keeps a
    /// `resolve` followed by `related` (or by a channel page) to one feed request.
    pub revalidate_after: Duration,
    /// Largest document (feed, page, oEmbed JSON) read into memory.
    pub max_document_bytes: usize,
    /// Number of documents kept in the cache.
    pub cache_entries: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(10),
            revalidate_after: Duration::from_secs(60),
            max_document_bytes: 8 * 1024 * 1024,
            cache_entries: 128,
        }
    }
}

/// What a document request expects, which sets its `Accept` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// RSS, Atom or JSON Feed.
    Feed,
    /// An HTML page.
    Page,
    /// A JSON document (oEmbed).
    Json,
}

/// A text document, fresh or from the cache.
#[derive(Debug, Clone)]
pub struct Document {
    /// URL after redirects.
    pub url: Url,
    /// `Content-Type`, when the server sent one.
    pub content_type: Option<String>,
    /// The body, decoded to text.
    pub body: Arc<str>,
}

/// The answer to a page request.
#[derive(Debug, Clone)]
pub enum PageResponse {
    /// A text document (HTML, usually).
    Document(Document),
    /// The URL serves media directly; its body was not read.
    Media {
        /// URL after redirects.
        url: Url,
        /// `Content-Type` of the media.
        content_type: String,
    },
}

/// What a ranged probe of a media URL found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// What the URL serves.
    pub class: MediaClass,
    /// `Content-Type` as sent.
    pub content_type: String,
    /// Full size of the file, from `Content-Range` (or `Content-Length` when the server
    /// ignored the range).
    pub size: Option<u64>,
}

#[derive(Debug)]
struct CacheEntry {
    etag: Option<HeaderValue>,
    last_modified: Option<HeaderValue>,
    checked_at: Instant,
    document: Document,
}

/// The shared HTTP client. Cheap to clone; clones share the connection pool and the cache.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
    config: Arc<HttpConfig>,
    cache: Arc<Mutex<HashMap<Url, CacheEntry>>>,
}

impl Http {
    /// Builds the client: rustls with the `ring` provider and the platform's certificate
    /// verifier, gzip, HTTP/2, at most 10 redirects.
    pub fn new(config: HttpConfig) -> Result<Self> {
        use rustls_platform_verifier::BuilderVerifierExt;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(BuilderVerifierExt::with_platform_verifier)
            .map_err(|e| Error::Service(format!("TLS setup failed: {e}")))?
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| Error::Service(format!("HTTP client setup failed: {e}")))?;
        Ok(Self {
            client,
            config: Arc::new(config),
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The underlying client, for resolvers that need a request this type does not offer.
    #[must_use]
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Fetches a text document, through the cache.
    ///
    /// A cached copy younger than [`HttpConfig::revalidate_after`] is returned as is; an older
    /// one is revalidated with `If-None-Match` / `If-Modified-Since`, and reused on
    /// `304 Not Modified`.
    pub async fn document(&self, url: &Url, kind: DocumentKind) -> Result<Document> {
        match self.page_or_document(url, kind).await? {
            PageResponse::Document(doc) => Ok(doc),
            PageResponse::Media { content_type, .. } => Err(Error::Parse(format!(
                "{url} serves {content_type}, not a document"
            ))),
        }
    }

    /// Fetches a page, through the cache, without reading the body when it turns out to be
    /// media (a feed link that points straight at the video file).
    pub async fn page(&self, url: &Url) -> Result<PageResponse> {
        self.page_or_document(url, DocumentKind::Page).await
    }

    async fn page_or_document(&self, url: &Url, kind: DocumentKind) -> Result<PageResponse> {
        let mut conditional = HeaderMap::new();
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = cache.get(url) {
                if entry.checked_at.elapsed() < self.config.revalidate_after {
                    return Ok(PageResponse::Document(entry.document.clone()));
                }
                if let Some(etag) = &entry.etag {
                    conditional.insert(header::IF_NONE_MATCH, etag.clone());
                }
                if let Some(modified) = &entry.last_modified {
                    conditional.insert(header::IF_MODIFIED_SINCE, modified.clone());
                }
            }
        }

        let accept = match kind {
            DocumentKind::Feed => ACCEPT_FEED,
            DocumentKind::Page => ACCEPT_PAGE,
            DocumentKind::Json => "application/json, */*;q=0.5",
        };
        let response = self
            .client
            .get(url.clone())
            .header(header::ACCEPT, accept)
            .headers(conditional)
            .send()
            .await
            .map_err(network)?;

        if response.status() == StatusCode::NOT_MODIFIED {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = cache.get_mut(url) {
                entry.checked_at = Instant::now();
                return Ok(PageResponse::Document(entry.document.clone()));
            }
            return Err(Error::Parse(format!(
                "{url} answered 304 to an unconditional request"
            )));
        }
        let response = check_status(response)?;
        let final_url = response.url().clone();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if kind == DocumentKind::Page
            && let Some(ct) = &content_type
            && (media::classify_mime(ct).is_some() || ct.starts_with("audio/"))
        {
            return Ok(PageResponse::Media {
                url: final_url,
                content_type: ct.clone(),
            });
        }
        let etag = response.headers().get(header::ETAG).cloned();
        let last_modified = response.headers().get(header::LAST_MODIFIED).cloned();
        let body = self.read_text(response).await?;
        let document = Document {
            url: final_url,
            content_type,
            body: body.into(),
        };
        self.store(
            url.clone(),
            CacheEntry {
                etag,
                last_modified,
                checked_at: Instant::now(),
                document: document.clone(),
            },
        );
        Ok(PageResponse::Document(document))
    }

    /// Checks that a URL serves video, with a `GET` for its first byte (`Range: bytes=0-0`).
    ///
    /// `GET` rather than `HEAD`: some CDNs refuse `HEAD` but serve ranges. Returns `None` when
    /// the URL fails or serves something else than video, HLS or DASH. A generic binary type
    /// (`application/octet-stream`) is accepted only for URLs with a video file extension.
    pub async fn probe(&self, url: &Url) -> Option<Probe> {
        let response = self
            .client
            .get(url.clone())
            .header(header::RANGE, "bytes=0-0")
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .inspect_err(|e| tracing::debug!(%url, error = %e, "probe failed"))
            .ok()?;
        let status = response.status();
        if !(status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT) {
            tracing::debug!(%url, %status, "probe rejected");
            return None;
        }
        let headers = response.headers();
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let class = media::classify_mime(&content_type).or_else(|| {
            media::is_generic_binary(&content_type)
                .then(|| media::classify_url(response.url()))
                .flatten()
        });
        let Some(class) = class else {
            tracing::debug!(%url, %content_type, "probe: not video");
            return None;
        };
        let size = if status == StatusCode::PARTIAL_CONTENT {
            headers
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit_once('/'))
                .and_then(|(_, total)| total.trim().parse().ok())
        } else {
            response.content_length()
        };
        // Dropping the response closes the stream without reading the rest of the file.
        Some(Probe {
            class,
            content_type,
            size,
        })
    }

    async fn read_text(&self, mut response: Response) -> Result<String> {
        let charset = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|ct| {
                ct.split(';').find_map(|p| {
                    let (k, v) = p.split_once('=')?;
                    k.trim()
                        .eq_ignore_ascii_case("charset")
                        .then(|| v.trim().trim_matches('"').to_ascii_lowercase())
                })
            });
        let max = self.config.max_document_bytes;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network)? {
            if bytes.len() + chunk.len() > max {
                return Err(Error::Parse(format!("document larger than {max} bytes")));
            }
            bytes.extend_from_slice(&chunk);
        }
        // Feeds and pages are UTF-8 nearly always; Latin-1 is the one legacy charset worth
        // decoding by hand. XML declarations are left to the feed parser.
        Ok(match charset.as_deref() {
            Some("iso-8859-1" | "latin1" | "windows-1252") => {
                bytes.iter().map(|&b| char::from(b)).collect()
            }
            _ => String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    fn store(&self, url: Url, entry: CacheEntry) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= self.config.cache_entries
            && !cache.contains_key(&url)
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, e)| e.checked_at)
                .map(|(u, _)| u.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(url, entry);
    }
}

fn network(error: reqwest::Error) -> Error {
    Error::Network(error.without_url().to_string())
}

fn check_status(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => Error::NotFound,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => Error::RateLimited {
            retry_after: response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok()),
        },
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Error::Unavailable(format!("the server refused access ({status})"))
        }
        _ => Error::Service(format!("{} answered {status}", response.url())),
    })
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page resolvers: finding the video file behind a feed item.
//!
//! A [`Resolver`] reads what the feed attached to an item ([`FeedMedia`]) and the item's page
//! ([`PageDocument`]), and returns a [`Resolution`]: candidate media URLs and the metadata the
//! feed lacks (duration, thumbnails, upload date, sensitive-content marker).
//!
//! The [`ResolverChain`] runs **site resolvers** first (those whose [`Resolver::applies_to`]
//! accepts the page), then the **generic** ones, in this order:
//!
//! 1. [`enclosure`]: RSS `<enclosure>`, Media RSS `media:content`, Atom `rel="enclosure"`;
//! 2. [`jsonld`]: JSON-LD `VideoObject`;
//! 3. [`microdata`]: schema.org `VideoObject` microdata;
//! 4. [`meta`]: Open Graph `og:video`, Twitter `twitter:player:stream`, `<meta name="rating">`;
//! 5. [`html5`]: `<video>` / `<source>`;
//! 6. [`oembed`]: the page's oEmbed document, only when nothing before found a candidate.
//!
//! Results are merged field by field, earlier resolvers first. Every candidate URL is then
//! **verified** with a ranged `GET` (`Range: bytes=0-0`) and kept only if it serves video, HLS
//! or DASH; nothing a page claims is trusted before that.

pub mod enclosure;
pub mod html5;
pub mod jsonld;
pub mod koreus;
pub mod meta;
pub mod microdata;
pub mod oembed;
mod schema;

use std::sync::Arc;

use async_trait::async_trait;
use flaneur_connector::{Error, Result};
use flaneur_model::{Item, Published, Seconds, Stream, StreamKind, Thumbnail};
use futures_util::{StreamExt, stream};
use scraper::Html;
use url::Url;

use crate::http::{Http, PageResponse, Probe};
use crate::media::{self, MediaClass};

/// Most candidate URLs probed for one item.
const MAX_PROBES: usize = 8;

/// Probes in flight at once for one item.
const PROBE_CONCURRENCY: usize = 2;

/// A media file the feed attached to an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedMedia {
    /// File URL.
    pub url: Url,
    /// MIME type as the feed gives it, parameters included.
    pub mime: Option<String>,
    /// Width in pixels.
    pub width: Option<u32>,
    /// Height in pixels.
    pub height: Option<u32>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// Duration.
    pub duration: Option<Seconds>,
}

impl FeedMedia {
    /// A media URL with nothing else known.
    #[must_use]
    pub fn url(url: Url) -> Self {
        Self {
            url,
            mime: None,
            width: None,
            height: None,
            size: None,
            duration: None,
        }
    }

    /// The feed says (by MIME type, or by extension when the type is missing or generic) that
    /// this is video.
    #[must_use]
    pub fn is_video(&self) -> bool {
        match self.mime.as_deref() {
            Some(mime) if !media::is_generic_binary(mime) => media::classify_mime(mime).is_some(),
            _ => media::classify_url(&self.url).is_some(),
        }
    }
}

/// An item's page, as fetched.
#[derive(Debug, Clone)]
pub struct PageDocument {
    /// URL after redirects; relative links resolve against it.
    pub url: Url,
    /// The HTML source.
    pub source: Arc<str>,
}

impl PageDocument {
    /// Parses the page. `Html` is not `Send`: parse inside a synchronous block and drop it before
    /// any `.await`.
    #[must_use]
    pub fn html(&self) -> Html {
        Html::parse_document(&self.source)
    }

    /// Resolves a link of the page.
    #[must_use]
    pub fn resolve(&self, href: &str) -> Option<Url> {
        media::resolve_url(Some(&self.url), href)
    }
}

/// What a resolver gets.
#[derive(Debug, Clone, Copy)]
pub struct ResolveRequest<'a> {
    /// The item's page (feed `<link>`), when it has one.
    pub page_url: Option<&'a Url>,
    /// What the feed attached to the item.
    pub feed_media: &'a [FeedMedia],
    /// The fetched page, when there is one and it is HTML.
    pub page: Option<&'a PageDocument>,
    /// The connector's HTTP client, for resolvers that need another document.
    pub http: &'a Http,
}

/// A media URL found by a resolver, not yet verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// File or manifest URL.
    pub url: Url,
    /// MIME type, with a `codecs` parameter when known.
    pub mime: Option<String>,
    /// The site's label of the variant ("HD", "vp09").
    pub label: Option<String>,
    /// Video width in pixels.
    pub width: Option<u32>,
    /// Video height in pixels.
    pub height: Option<u32>,
    /// Average bitrate in bit/s.
    pub bitrate: Option<u32>,
    /// Size in bytes.
    pub size: Option<u64>,
}

impl Candidate {
    /// A candidate with a URL and a MIME type.
    #[must_use]
    pub fn new(url: Url, mime: Option<String>) -> Self {
        Self {
            url,
            mime: mime.filter(|m| !m.trim().is_empty()),
            label: None,
            width: None,
            height: None,
            bitrate: None,
            size: None,
        }
    }

    /// The candidate's declared type rules it out (an HTML embed page, an image...).
    #[must_use]
    pub fn declared_not_video(&self) -> bool {
        self.mime
            .as_deref()
            .is_some_and(|m| !media::is_generic_binary(m) && media::classify_mime(m).is_none())
    }

    fn fill_from(&mut self, other: Self) {
        self.mime = self.mime.take().or(other.mime);
        self.label = self.label.take().or(other.label);
        self.width = self.width.or(other.width);
        self.height = self.height.or(other.height);
        self.bitrate = self.bitrate.or(other.bitrate);
        self.size = self.size.or(other.size);
    }
}

/// What a resolver found. Every field is optional; the chain merges them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolution {
    /// Media URLs, best first.
    pub candidates: Vec<Candidate>,
    /// Title.
    pub title: Option<String>,
    /// Plain-text description.
    pub description: Option<String>,
    /// Duration.
    pub duration: Option<Seconds>,
    /// Upload date.
    pub published: Option<Published>,
    /// Thumbnails.
    pub thumbnails: Vec<Thumbnail>,
    /// Sensitive-content marker: `Some(true)` for adult or mature content, `Some(false)` when
    /// the page says it is for a general audience, `None` when it says nothing.
    pub sensitive: Option<bool>,
}

impl Resolution {
    /// Merges a later result into this one: scalar fields keep the earlier value, lists are
    /// joined, candidates and thumbnails with the same URL are merged, and `sensitive` folds
    /// with [`media::combine_sensitive`].
    pub fn merge(&mut self, other: Self) {
        for candidate in other.candidates {
            self.add_candidate(candidate);
        }
        self.title = self.title.take().or(other.title);
        self.description = self.description.take().or(other.description);
        self.duration = self.duration.or(other.duration);
        self.published = self.published.or(other.published);
        media::merge_thumbnails(&mut self.thumbnails, other.thumbnails);
        self.sensitive = media::combine_sensitive([self.sensitive, other.sensitive]);
    }

    /// Adds a candidate, merging it with an earlier one of the same URL.
    pub fn add_candidate(&mut self, candidate: Candidate) {
        if let Some(existing) = self.candidates.iter_mut().find(|c| c.url == candidate.url) {
            existing.fill_from(candidate);
        } else {
            self.candidates.push(candidate);
        }
    }
}

/// A URL a site resolver recognizes (see [`Resolver::site_url`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteUrl {
    /// The site's video feed.
    Feed(Url),
    /// A video page of the site.
    Item {
        /// The feed the item appears in, when the site has one.
        feed: Option<Url>,
        /// The page, in its canonical form (the same URL the feed links to).
        page: Url,
    },
}

/// Finds media and metadata for a feed item. Implement it to support a site whose pages the
/// generic resolvers do not read well, and register it with
/// [`RssConnectorBuilder::resolver`](crate::RssConnectorBuilder::resolver).
#[async_trait]
pub trait Resolver: Send + Sync {
    /// Short name, for logs ("koreus", "json-ld").
    fn name(&self) -> &'static str;

    /// Whether the resolver should run for this page. Generic resolvers accept everything; a
    /// site resolver checks the host.
    fn applies_to(&self, page: Option<&Url>) -> bool {
        let _ = page;
        true
    }

    /// A fallback resolver runs only when the earlier ones found no candidate, because it costs
    /// a request of its own.
    fn fallback(&self) -> bool {
        false
    }

    /// When an authoritative resolver finds candidates, the candidates of the resolvers after
    /// it are ignored (their metadata is still merged). A site resolver that knows the exact
    /// variants of its pages sets it, so a generic resolver cannot add a wrong size to them.
    fn authoritative(&self) -> bool {
        false
    }

    /// Reads the request and returns what it found; an empty [`Resolution`] when nothing.
    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution>;

    /// Fixes an item mapped from a feed of the site, before it is listed (broken thumbnail
    /// URLs, for instance). Called only when [`Resolver::applies_to`] accepts the item's page.
    fn fix_item(&self, item: &mut Item) {
        let _ = item;
    }

    /// Recognizes a URL of the site, for `parse_url`: its feed, or one of its video pages.
    fn site_url(&self, url: &Url) -> Option<SiteUrl> {
        let _ = url;
        None
    }
}

/// What the chain resolved for one item, verified.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    /// Merged metadata; its `candidates` are the ones that passed verification.
    pub resolution: Resolution,
    /// Verified progressive files, as muxed streams, best first.
    pub streams: Vec<Stream>,
    /// Verified HLS playlist.
    pub hls: Option<Url>,
    /// Verified DASH manifest.
    pub dash: Option<Url>,
}

impl Resolved {
    /// Nothing playable was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty() && self.hls.is_none() && self.dash.is_none()
    }
}

/// Site resolvers, then generic resolvers, run in order.
#[derive(Clone)]
pub struct ResolverChain {
    site: Vec<Arc<dyn Resolver>>,
    generic: Vec<Arc<dyn Resolver>>,
}

impl std::fmt::Debug for ResolverChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = |list: &[Arc<dyn Resolver>]| list.iter().map(|r| r.name()).collect::<Vec<_>>();
        f.debug_struct("ResolverChain")
            .field("site", &names(&self.site))
            .field("generic", &names(&self.generic))
            .finish()
    }
}

impl Default for ResolverChain {
    /// The generic resolvers and the built-in site resolvers.
    fn default() -> Self {
        let mut chain = Self::generic();
        chain.push_site(Arc::new(koreus::Koreus));
        chain
    }
}

impl ResolverChain {
    /// The generic resolvers only.
    #[must_use]
    pub fn generic() -> Self {
        Self {
            site: Vec::new(),
            generic: vec![
                Arc::new(enclosure::Enclosure),
                Arc::new(jsonld::JsonLd),
                Arc::new(microdata::Microdata),
                Arc::new(meta::MetaTags),
                Arc::new(html5::Html5Video),
                Arc::new(oembed::OEmbed),
            ],
        }
    }

    /// Adds a site resolver; site resolvers run in the order they were added, before the
    /// generic ones.
    pub fn push_site(&mut self, resolver: Arc<dyn Resolver>) {
        self.site.push(resolver);
    }

    /// The site resolvers.
    pub fn sites(&self) -> impl Iterator<Item = &dyn Resolver> {
        self.site.iter().map(AsRef::as_ref)
    }

    /// Applies the site fix-ups to an item mapped from a feed.
    pub fn fix_item(&self, item: &mut Item) {
        let page = item.web_url.clone();
        for resolver in &self.site {
            if resolver.applies_to(page.as_ref()) {
                resolver.fix_item(item);
            }
        }
    }

    /// Runs the chain for one item and verifies the candidates.
    ///
    /// The page is fetched once (through the HTTP cache) unless the link points at a media
    /// file. A page that fails to load is an error only when the feed gave no media either.
    pub async fn run(
        &self,
        http: &Http,
        page_url: Option<&Url>,
        feed_media: &[FeedMedia],
    ) -> Result<Resolved> {
        let mut merged = Resolution::default();
        let page = match page_url {
            Some(url)
                if media::classify_url(url).is_some()
                    || feed_media.iter().any(|m| &m.url == url) =>
            {
                merged.add_candidate(Candidate::new(url.clone(), None));
                None
            }
            Some(url) => match http.page(url).await {
                Ok(PageResponse::Document(doc)) => Some(PageDocument {
                    url: doc.url,
                    source: doc.body,
                }),
                Ok(PageResponse::Media { url, content_type }) => {
                    merged.add_candidate(Candidate::new(url, Some(content_type)));
                    None
                }
                Err(error) if !feed_media.is_empty() => {
                    tracing::warn!(%url, %error, "page failed to load; using the feed media");
                    None
                }
                Err(error) => return Err(error),
            },
            None => None,
        };
        let request = ResolveRequest {
            page_url,
            feed_media,
            page: page.as_ref(),
            http,
        };

        let site = self.site.iter().filter(|r| r.applies_to(page_url));
        let mut locked = false;
        for resolver in site.chain(&self.generic) {
            if resolver.fallback() && !merged.candidates.is_empty() {
                continue;
            }
            match resolver.resolve(&request).await {
                Ok(mut found) => {
                    if locked {
                        found.candidates.clear();
                    } else if resolver.authoritative() && !found.candidates.is_empty() {
                        // Candidates found before (the page link itself) stay; later ones do not.
                        locked = true;
                    }
                    tracing::debug!(
                        resolver = resolver.name(),
                        candidates = found.candidates.len(),
                        "resolved"
                    );
                    merged.merge(found);
                }
                Err(error) => {
                    tracing::warn!(resolver = resolver.name(), %error, "resolver failed");
                }
            }
        }

        Ok(verify(http, merged).await)
    }
}

/// Probes every candidate and turns those that serve video into streams or manifests.
async fn verify(http: &Http, mut resolution: Resolution) -> Resolved {
    let candidates: Vec<Candidate> = std::mem::take(&mut resolution.candidates)
        .into_iter()
        .filter(|c| !c.declared_not_video())
        .take(MAX_PROBES)
        .collect();
    // A few at a time: a burst of parallel requests is impolite, and some CDNs drop the
    // connections of a client that opens many at once.
    let urls: Vec<Url> = candidates.iter().map(|c| c.url.clone()).collect();
    let probes: Vec<Option<Probe>> = stream::iter(urls)
        .map(|url| {
            let http = http.clone();
            async move { http.probe(&url).await }
        })
        .buffered(PROBE_CONCURRENCY)
        .collect()
        .await;

    let duration = resolution.duration;
    let mut resolved = Resolved::default();
    for (candidate, probe) in candidates.into_iter().zip(probes) {
        let Some(probe) = probe else { continue };
        let url = candidate.url.clone();
        match probe.class {
            MediaClass::Hls => {
                resolved.hls.get_or_insert(url);
            }
            MediaClass::Dash => {
                resolved.dash.get_or_insert(url);
            }
            MediaClass::File => resolved
                .streams
                .push(to_stream(url, &candidate, &probe, duration)),
        }
        resolution.candidates.push(candidate);
    }
    resolved.resolution = resolution;
    resolved
}

fn to_stream(url: Url, candidate: &Candidate, probe: &Probe, duration: Option<Seconds>) -> Stream {
    let codecs = candidate.mime.as_deref().map_or_default(media::codecs);
    let mime = if media::is_generic_binary(&probe.content_type) {
        candidate.mime.as_deref()
    } else {
        Some(probe.content_type.as_str())
    };
    let size = probe.size.or(candidate.size);
    let bitrate = candidate.bitrate.or_else(|| {
        let (size, duration) = (size?, u64::from(duration?));
        (duration > 0)
            .then(|| size.saturating_mul(8) / duration)
            .and_then(|b| u32::try_from(b).ok())
    });
    Stream {
        container: media::container(mime, &url),
        url,
        kind: StreamKind::Muxed,
        bitrate,
        size,
        width: candidate.width,
        height: candidate.height,
        fps: None,
        video_codec: media::video_codec(&codecs),
        hdr: None,
        audio: media::audio_track(&codecs),
    }
}

/// An error for an item that has nothing playable.
pub(crate) fn nothing_playable() -> Error {
    Error::Unavailable("no playable video found for this item".to_owned())
}

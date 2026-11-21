// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recognizing feed URLs without fetching them.
//!
//! `parse_url` cannot make a request, so only URL shapes count: a page of an unknown site is
//! not claimed (it may belong to another connector, or have no video), and its feeds are found
//! with [`RssConnector::discover_feeds`](crate::RssConnector::discover_feeds) instead.

use url::Url;

/// Turns `feed:` URLs (`feed://host/path`, `feed:https://host/path`) into `http(s)` ones and
/// drops the fragment. `None` for any other scheme.
#[must_use]
pub fn normalize(url: &Url) -> Option<Url> {
    let mut url = match url.scheme() {
        "http" | "https" => url.clone(),
        "feed" => {
            let rest = &url.as_str()["feed:".len()..];
            if rest.starts_with("http://") || rest.starts_with("https://") {
                Url::parse(rest).ok()?
            } else {
                Url::parse(&format!("https:{rest}")).ok()?
            }
        }
        _ => return None,
    };
    url.set_fragment(None);
    Some(url)
}

/// Whether the URL has the shape of a feed: a `.rss` / `.atom` / `.xml` / `.rdf` file, a
/// `/feed`, `/rss` or `/atom` path segment, a feed host (`feeds.*`), or a `feed=` / `format=`
/// query asking for RSS or Atom.
#[must_use]
pub fn looks_like_feed(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    let file = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    if file.contains("sitemap") {
        return false;
    }
    let by_extension = [".rss", ".atom", ".xml", ".rdf"]
        .iter()
        .any(|ext| file.ends_with(ext));
    let by_segment = path
        .split('/')
        .any(|s| matches!(s, "feed" | "feeds" | "rss" | "atom" | "rss2"));
    let by_host = url.host_str().is_some_and(|h| {
        h.starts_with("feeds.") || h.starts_with("feed.") || h.starts_with("rss.")
    });
    let by_query = url.query_pairs().any(|(k, v)| {
        matches!(k.as_ref(), "feed" | "format" | "type")
            && matches!(v.to_ascii_lowercase().as_str(), "rss" | "rss2" | "atom")
    });
    by_extension || by_segment || by_host || by_query
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(s: &str) -> bool {
        looks_like_feed(&Url::parse(s).unwrap())
    }

    #[test]
    fn recognizes_feed_shapes() {
        assert!(feed("https://example.org/videos.rss"));
        assert!(feed("https://example.org/blog/feed/"));
        assert!(feed("https://example.org/index.xml"));
        assert!(feed("https://feeds.feedburner.com/Something"));
        assert!(feed("https://example.org/?feed=rss2"));
        assert!(!feed("https://example.org/video/page.html"));
        assert!(!feed("https://example.org/sitemap.xml"));
    }

    #[test]
    fn normalizes_feed_scheme() {
        let n = |s: &str| normalize(&Url::parse(s).unwrap()).map(|u| u.to_string());
        assert_eq!(
            n("feed://example.org/a.rss").as_deref(),
            Some("https://example.org/a.rss")
        );
        assert_eq!(
            n("feed:http://example.org/a.rss#x").as_deref(),
            Some("http://example.org/a.rss")
        );
        assert_eq!(n("mailto:a@example.org"), None);
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! Koreus (`www.koreus.com`), a French site of humorous and unusual videos that hosts its own
//! MP4 files.
//!
//! The generic resolvers already find one file per page (feed `media:content`, microdata
//! `contentURL`, `og:video`), the duration (microdata) and the rating (`<meta name="rating">`).
//! This resolver adds what they cannot see:
//!
//! - **The variants.** The player is injected by a script, so its `<source>` elements (H.264
//!   `avc1`, VP9 `vp09`, and `avc1 HD` on older pages) are only in a JavaScript string
//!   (`var koreusPlayer='<video-js>…</video-js>'`), not in the DOM. Each becomes a candidate with
//!   its codecs. The page's `width` / `height` (microdata) describe the best variant: the `HD`
//!   one when there is one (the SD file is then smaller, of a size the page does not give), every
//!   variant otherwise.
//! - **Thumbnails.** The feed's `media:thumbnail` URLs lack the dot before the extension
//!   (`…/slugjpg`); [`Resolver::fix_item`] repairs them.
//! - **URLs.** Video pages (`/video/<slug>.html`, the old `/media/<slug>.html`) map to the items
//!   of the site's video feed.

use async_trait::async_trait;
use flaneur_connector::Result;
use scraper::Html;
use url::Url;

use super::{Candidate, PageDocument, Resolution, ResolveRequest, Resolver, SiteUrl, html5};
use crate::media;
use flaneur_model::Item;

/// The site's video feed.
pub const FEED: &str = "https://feeds.feedburner.com/Koreus-videos";

/// The Koreus site resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct Koreus;

fn is_koreus(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h == "koreus.com" || h.ends_with(".koreus.com"))
}

#[async_trait]
impl Resolver for Koreus {
    fn name(&self) -> &'static str {
        "koreus"
    }

    fn applies_to(&self, page: Option<&Url>) -> bool {
        page.is_some_and(is_koreus)
    }

    fn authoritative(&self) -> bool {
        true
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        let Some(page) = request.page else {
            return Ok(Resolution::default());
        };
        Ok(read(page))
    }

    fn fix_item(&self, item: &mut Item) {
        for thumb in &mut item.thumbnails {
            if let Some(fixed) = fix_thumbnail(&thumb.url) {
                thumb.url = fixed;
            }
        }
        let mut seen = Vec::new();
        item.thumbnails.retain(|t| {
            let new = !seen.contains(&t.url);
            seen.push(t.url.clone());
            new
        });
    }

    fn site_url(&self, url: &Url) -> Option<SiteUrl> {
        let feed = || Url::parse(FEED).expect("valid URL");
        if url.host_str() == Some("feeds.feedburner.com") {
            return url
                .path()
                .trim_end_matches('/')
                .eq_ignore_ascii_case("/Koreus-videos")
                .then(|| SiteUrl::Feed(feed()));
        }
        if !is_koreus(url) {
            return None;
        }
        let path = url.path();
        let slug = path
            .strip_prefix("/video/")
            .or_else(|| path.strip_prefix("/media/"))
            .map(|s| s.trim_end_matches('/').trim_end_matches(".html"))
            .filter(|s| !s.is_empty() && !s.contains('/'));
        match slug {
            Some(slug) => Some(SiteUrl::Item {
                feed: Some(feed()),
                page: Url::parse(&format!("https://www.koreus.com/video/{slug}.html")).ok()?,
            }),
            None if path == "/" || path.starts_with("/videos") => Some(SiteUrl::Feed(feed())),
            None => None,
        }
    }
}

/// Reads the player variants of a video page.
#[must_use]
pub fn read(page: &PageDocument) -> Resolution {
    let Some(player) = player_markup(&page.source) else {
        return Resolution::default();
    };
    let fragment = Html::parse_fragment(&player);
    let mut variants = html5::read(&fragment, page);

    // The page's size is the size of its best variant.
    let (width, height) = page_size(&page.html());
    let has_hd = variants
        .candidates
        .iter()
        .any(|c| c.label.as_deref().is_some_and(is_hd_label));
    for candidate in &mut variants.candidates {
        let hd = candidate.label.as_deref().is_some_and(is_hd_label);
        if hd || !has_hd {
            candidate.width = width;
            candidate.height = height;
        }
    }
    // Best first: HD, then the others in page order (VP9 before H.264 on current pages).
    variants
        .candidates
        .sort_by_key(|c: &Candidate| !c.label.as_deref().is_some_and(is_hd_label));
    variants
}

fn is_hd_label(label: &str) -> bool {
    label
        .split_whitespace()
        .any(|w| w.eq_ignore_ascii_case("hd"))
}

/// The HTML in `var koreusPlayer='…'`.
fn player_markup(source: &str) -> Option<String> {
    let start = source.find("koreusPlayer='")? + "koreusPlayer='".len();
    let rest = &source[start..];
    // The string ends at the first quote that is not escaped.
    let bytes = rest.as_bytes();
    let end = (0..bytes.len()).find(|&i| bytes[i] == b'\'' && (i == 0 || bytes[i - 1] != b'\\'))?;
    Some(rest[..end].replace("\\'", "'"))
}

fn page_size(html: &Html) -> (Option<u32>, Option<u32>) {
    let selector =
        |prop: &str| media::selector(&format!(r#"[itemscope] meta[itemprop="{prop}"][content]"#));
    let read = |prop: &str| {
        html.select(&selector(prop))
            .next()
            .and_then(|m| m.value().attr("content")?.trim().parse().ok())
    };
    (read("width"), read("height"))
}

/// `…/slugjpg` → `…/slug.jpg`.
fn fix_thumbnail(url: &Url) -> Option<Url> {
    if !is_koreus_cdn(url) {
        return None;
    }
    let path = url.path();
    let file = path.rsplit('/').next()?;
    if file.contains('.') {
        return None;
    }
    let ext = ["jpg", "jpeg", "png", "webp"]
        .into_iter()
        .find(|ext| file.len() > ext.len() && file.ends_with(ext))?;
    let mut fixed = url.clone();
    fixed.set_path(&format!("{}.{ext}", &path[..path.len() - ext.len()]));
    Some(fixed)
}

fn is_koreus_cdn(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h.starts_with("koreus") || h.ends_with(".koreus.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_thumbnails_without_dot() {
        let broken =
            Url::parse("https://koreus.b-cdn.net/thumbs/202611/illusion-tandemjpg").unwrap();
        assert_eq!(
            fix_thumbnail(&broken).unwrap().as_str(),
            "https://koreus.b-cdn.net/thumbs/202611/illusion-tandem.jpg"
        );
        let fine = Url::parse("https://koreus.b-cdn.net/thumbshigh/202611/x.jpg").unwrap();
        assert_eq!(fix_thumbnail(&fine), None);
        let other = Url::parse("https://example.org/thumbs/xjpg").unwrap();
        assert_eq!(fix_thumbnail(&other), None);
    }

    #[test]
    fn recognizes_site_urls() {
        let item = |u: &str| Koreus.site_url(&Url::parse(u).unwrap());
        let page = Url::parse("https://www.koreus.com/video/illusion-tandem.html").unwrap();
        let expected = Some(SiteUrl::Item {
            feed: Some(Url::parse(FEED).unwrap()),
            page,
        });
        assert_eq!(
            item("https://www.koreus.com/video/illusion-tandem.html"),
            expected
        );
        assert_eq!(
            item("http://koreus.com/media/illusion-tandem.html"),
            expected
        );
        assert_eq!(
            item("https://www.koreus.com/video/illusion-tandem"),
            expected
        );
        assert_eq!(
            item("http://feeds.feedburner.com/Koreus-videos"),
            Some(SiteUrl::Feed(Url::parse(FEED).unwrap()))
        );
        assert_eq!(
            item("https://www.koreus.com/"),
            Some(SiteUrl::Feed(Url::parse(FEED).unwrap()))
        );
        assert_eq!(item("https://www.koreus.com/forum/x.html"), None);
        assert_eq!(item("https://feeds.feedburner.com/Other"), None);
    }

    #[test]
    fn extracts_player_markup() {
        let source = "var koreusPlayer='<video-js><source src=\"a.mp4\"></video-js>'\nif (x) {}";
        assert_eq!(
            player_markup(source).unwrap(),
            "<video-js><source src=\"a.mp4\"></video-js>"
        );
        assert_eq!(player_markup("nothing"), None);
    }
}

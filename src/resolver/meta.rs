// SPDX-License-Identifier: AGPL-3.0-or-later

//! `<meta>` tags: Open Graph (`og:video`, `og:image`, `og:title`, `video:duration`...), the
//! Twitter card stream (`twitter:player:stream`), `<meta name="rating">`, and `<title>` as the
//! last resort for a title.

use async_trait::async_trait;
use flaneur_connector::Result;
use flaneur_model::Thumbnail;

use super::{Candidate, PageDocument, Resolution, ResolveRequest, Resolver};
use crate::media;

/// The meta tags resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetaTags;

#[async_trait]
impl Resolver for MetaTags {
    fn name(&self) -> &'static str {
        "meta-tags"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        Ok(request.page.map_or_default(read))
    }
}

#[derive(Default)]
struct Group {
    url: Option<String>,
    secure_url: Option<String>,
    mime: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Group {
    fn href(&self) -> Option<&str> {
        self.secure_url.as_deref().or(self.url.as_deref())
    }
}

/// Reads the `<meta>` tags of a page.
#[must_use]
pub fn read(page: &PageDocument) -> Resolution {
    let html = page.html();
    let selector = media::selector("meta[content]");
    let mut videos: Vec<Group> = Vec::new();
    let mut images: Vec<Group> = Vec::new();
    let mut resolution = Resolution::default();
    let mut title = None;
    let mut stream = None;
    let mut sensitive = Vec::new();

    for el in html.select(&selector) {
        let key = el
            .value()
            .attr("property")
            .or_else(|| el.value().attr("name"))
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let value = el.value().attr("content").unwrap_or("").trim().to_owned();
        if value.is_empty() {
            continue;
        }
        let dim = || value.parse::<u32>().ok().filter(|v| *v > 0);
        match key.as_str() {
            "og:video" | "og:video:url" => open_group(&mut videos, value),
            "og:video:secure_url" => last(&mut videos).secure_url = Some(value),
            "og:video:type" => last(&mut videos).mime = Some(value),
            "og:video:width" => last(&mut videos).width = dim(),
            "og:video:height" => last(&mut videos).height = dim(),
            "og:image" | "og:image:url" => open_group(&mut images, value),
            "og:image:secure_url" => last(&mut images).secure_url = Some(value),
            "og:image:width" => last(&mut images).width = dim(),
            "og:image:height" => last(&mut images).height = dim(),
            "og:title" => title = Some(value),
            "og:description" => resolution.description = Some(media::html_to_text(&value)),
            "video:duration" | "og:video:duration" => {
                resolution.duration = media::parse_duration(&value);
            }
            "video:release_date" => resolution.published = media::parse_date(&value),
            "twitter:player:stream" => stream = Some(value),
            "twitter:player:stream:content_type" => {
                if let Some(s) = stream.take() {
                    resolution.add_candidate_href(page, &s, Some(value));
                }
            }
            "rating" => sensitive.push(media::rating_to_sensitive(&value)),
            "og:restrictions:age" => {
                let age = value.trim_end_matches('+').trim().parse::<u32>().ok();
                sensitive.push(age.filter(|a| *a >= 18).map(|_| true));
            }
            _ => {}
        }
    }

    for group in &videos {
        if let Some(href) = group.href()
            && let Some(url) = page.resolve(href)
        {
            let mut candidate = Candidate::new(url, group.mime.clone());
            candidate.width = group.width;
            candidate.height = group.height;
            resolution.add_candidate(candidate);
        }
    }
    if let Some(s) = stream {
        resolution.add_candidate_href(page, &s, None);
    }
    let thumbnails = images.iter().filter_map(|g| {
        Some(Thumbnail {
            url: page.resolve(g.href()?)?,
            width: g.width,
            height: g.height,
        })
    });
    media::merge_thumbnails(&mut resolution.thumbnails, thumbnails);
    resolution.sensitive = media::combine_sensitive(sensitive);
    resolution.title = title.or_else(|| {
        let selector = media::selector("title");
        html.select(&selector)
            .next()
            .map(|t| t.text().collect::<String>().trim().to_owned())
    });
    resolution.title = resolution.title.filter(|t| !t.is_empty());
    resolution
}

/// Starts a new structured group for an `og:video` / `og:image` URL, unless it repeats the URL
/// of the current group (`og:video` followed by `og:video:url`), or fills a group that its
/// properties opened before any URL.
fn open_group(groups: &mut Vec<Group>, url: String) {
    match groups.last_mut() {
        Some(group) if group.url.as_deref() == Some(url.as_str()) => {}
        Some(group) if group.url.is_none() => group.url = Some(url),
        _ => groups.push(Group {
            url: Some(url),
            ..Group::default()
        }),
    }
}

/// The current group, opened if there is none.
fn last(groups: &mut Vec<Group>) -> &mut Group {
    if groups.is_empty() {
        groups.push(Group::default());
    }
    let index = groups.len() - 1;
    &mut groups[index]
}

impl Resolution {
    fn add_candidate_href(&mut self, page: &PageDocument, href: &str, mime: Option<String>) {
        if let Some(url) = page.resolve(href) {
            self.add_candidate(Candidate::new(url, mime));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_og_video_url_stays_one_group() {
        let page = PageDocument {
            url: url::Url::parse("https://example.org/v/1").unwrap(),
            source: r#"<meta property="og:video" content="/a.mp4">
                <meta property="og:video:url" content="/a.mp4">
                <meta property="og:video:type" content="video/mp4">
                <meta property="og:video" content="/b.mp4">
                <meta property="og:restrictions:age" content="18+">"#
                .into(),
        };
        let found = read(&page);
        assert_eq!(found.candidates.len(), 2);
        assert_eq!(found.candidates[0].mime.as_deref(), Some("video/mp4"));
        assert_eq!(
            found.candidates[1].url.as_str(),
            "https://example.org/b.mp4"
        );
        assert_eq!(found.candidates[1].mime, None);
        assert_eq!(found.sensitive, Some(true));
    }
}

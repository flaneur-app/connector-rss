// SPDX-License-Identifier: AGPL-3.0-or-later

//! HTML5 players: `<video src>`, `<video><source src type>`, and the same in Video.js's
//! `<video-js>` element.

use async_trait::async_trait;
use flaneur_connector::Result;
use flaneur_model::Thumbnail;
use scraper::Html;

use super::{Candidate, PageDocument, Resolution, ResolveRequest, Resolver};
use crate::media;

/// The `<video>` resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct Html5Video;

#[async_trait]
impl Resolver for Html5Video {
    fn name(&self) -> &'static str {
        "html5-video"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        Ok(request.page.map_or_default(|page| read(&page.html(), page)))
    }
}

/// Reads the `<video>` elements of a parsed document or fragment.
#[must_use]
pub fn read(html: &Html, page: &PageDocument) -> Resolution {
    let players = media::selector("video, video-js");
    let sources = media::selector("source[src]");
    let mut resolution = Resolution::default();
    for player in html.select(&players) {
        if let Some(src) = player.value().attr("src")
            && let Some(url) = page.resolve(src)
        {
            resolution.add_candidate(Candidate::new(url, None));
        }
        for source in player.select(&sources) {
            let attr = |name| source.value().attr(name).map(str::to_owned);
            let Some(url) = attr("src").and_then(|s| page.resolve(&s)) else {
                continue;
            };
            let mut candidate = Candidate::new(url, attr("type"));
            candidate.label = attr("label").or_else(|| attr("title"));
            let dim = |name| attr(name).and_then(|v| v.trim().parse::<u32>().ok());
            // Some players put the variant's height on the source (`size="720"`, `res="720"`).
            candidate.height = dim("size").or_else(|| dim("res")).or_else(|| dim("height"));
            resolution.add_candidate(candidate);
        }
        if let Some(poster) = player.value().attr("poster")
            && let Some(url) = page.resolve(poster)
        {
            media::merge_thumbnails(
                &mut resolution.thumbnails,
                [Thumbnail {
                    url,
                    width: None,
                    height: None,
                }],
            );
        }
    }
    resolution
}

// SPDX-License-Identifier: AGPL-3.0-or-later

//! The media the feed attached to the item: RSS `<enclosure>`, Media RSS `media:content`, Atom
//! `<link rel="enclosure">`. Needs no page.

use async_trait::async_trait;
use flaneur_connector::Result;

use super::{Candidate, Resolution, ResolveRequest, Resolver};

/// The feed enclosure resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct Enclosure;

#[async_trait]
impl Resolver for Enclosure {
    fn name(&self) -> &'static str {
        "enclosure"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        let mut resolution = Resolution::default();
        for media in request.feed_media.iter().filter(|m| m.is_video()) {
            let mut candidate = Candidate::new(media.url.clone(), media.mime.clone());
            candidate.width = media.width;
            candidate.height = media.height;
            candidate.size = media.size;
            resolution.add_candidate(candidate);
            resolution.duration = resolution.duration.or(media.duration);
        }
        Ok(resolution)
    }
}

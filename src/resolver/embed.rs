// SPDX-License-Identifier: AGPL-3.0-or-later

//! Players of other services embedded in the page, for `Playable::handoff`.
//!
//! Only YouTube embeds are recognized (`youtube.com/embed/<id>`, `youtube-nocookie.com/embed/<id>`),
//! because their id is the 11-character video id the YouTube connector uses, readable without a
//! request. The raw page source is scanned, so players written into the page by a script count
//! too. The chain uses the handoff only when no candidate of this page plays.

use async_trait::async_trait;
use flaneur_connector::Result;
use flaneur_model::{ItemRef, ServiceId};

use super::{Resolution, ResolveRequest, Resolver};

/// The embedded players resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmbeddedPlayer;

#[async_trait]
impl Resolver for EmbeddedPlayer {
    fn name(&self) -> &'static str {
        "embedded-player"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        Ok(Resolution {
            handoff: request.page.and_then(|page| handoff(&page.source)),
            ..Resolution::default()
        })
    }
}

/// The first YouTube video embedded in an HTML (or script) source.
#[must_use]
pub fn handoff(source: &str) -> Option<ItemRef> {
    const MARKERS: [&str; 2] = ["youtube.com/embed/", "youtube-nocookie.com/embed/"];
    MARKERS
        .iter()
        .filter_map(|marker| {
            source.match_indices(marker).find_map(|(at, _)| {
                let id = youtube_id(&source[at + marker.len()..])?;
                Some((at, id))
            })
        })
        .min_by_key(|(at, _)| *at)
        .map(|(_, id)| ItemRef::new(ServiceId::new("youtube"), id))
}

/// An 11-character YouTube id at the start of `rest`, followed by a non-id character.
fn youtube_id(rest: &str) -> Option<&str> {
    let is_id = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let len = rest.chars().take_while(|c| is_id(*c)).count();
    (len == 11).then(|| &rest[..11])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_youtube_embeds() {
        let src = r#"x.innerHTML='<iframe src="//www.youtube.com/embed/hH7HeBpPbQA?rel=0">';"#;
        assert_eq!(handoff(src).unwrap().to_string(), "youtube:hH7HeBpPbQA");
        let nocookie = r#"<iframe src="https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ">"#;
        assert_eq!(handoff(nocookie).unwrap().id, "dQw4w9WgXcQ");
        assert_eq!(
            handoff(r#"<a href="https://youtube.com/embed/short">"#),
            None
        );
        assert_eq!(handoff("<video src=a.mp4>"), None);
    }
}

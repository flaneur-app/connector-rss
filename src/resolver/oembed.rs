// SPDX-License-Identifier: AGPL-3.0-or-later

//! oEmbed: the JSON document a page links with
//! `<link rel="alternate" type="application/json+oembed">`.
//!
//! oEmbed rarely gives a direct media URL (its `html` is usually an `<iframe>`), so this
//! resolver mostly brings a title and a sized thumbnail. It costs a request, so the chain runs
//! it only when the earlier resolvers found no candidate.

use async_trait::async_trait;
use flaneur_connector::Result;
use flaneur_model::Thumbnail;
use scraper::Html;
use serde_json::Value;

use super::{PageDocument, Resolution, ResolveRequest, Resolver, embed, html5};
use crate::http::DocumentKind;
use crate::media;

/// The oEmbed resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct OEmbed;

#[async_trait]
impl Resolver for OEmbed {
    fn name(&self) -> &'static str {
        "oembed"
    }

    fn fallback(&self) -> bool {
        true
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        let Some(page) = request.page else {
            return Ok(Resolution::default());
        };
        let endpoint = {
            let html = page.html();
            let selector = media::selector(
                r#"link[rel~="alternate"][type="application/json+oembed" i][href]"#,
            );
            html.select(&selector)
                .next()
                .and_then(|l| page.resolve(l.value().attr("href")?))
        };
        let Some(endpoint) = endpoint else {
            return Ok(Resolution::default());
        };
        let doc = request.http.document(&endpoint, DocumentKind::Json).await?;
        let value: Value = serde_json::from_str(&doc.body)
            .map_err(|e| flaneur_connector::Error::Parse(format!("oEmbed: {e}")))?;
        Ok(read(&value, page))
    }
}

fn read(value: &Value, page: &PageDocument) -> Resolution {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| {
        value.get(key).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str()?.parse().ok())
                .and_then(|n| u32::try_from(n).ok())
        })
    };
    let mut resolution = match text("html") {
        Some(html) => {
            let mut found = html5::read(&Html::parse_fragment(&html), page);
            found.handoff = embed::handoff(&html);
            found
        }
        None => Resolution::default(),
    };
    resolution.title = text("title").filter(|t| !t.is_empty());
    if let Some(url) = text("thumbnail_url").and_then(|u| page.resolve(&u)) {
        media::merge_thumbnails(
            &mut resolution.thumbnails,
            [Thumbnail {
                url,
                width: number("thumbnail_width"),
                height: number("thumbnail_height"),
            }],
        );
    }
    resolution.duration = number("duration");
    resolution
}

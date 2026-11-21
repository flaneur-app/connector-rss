// SPDX-License-Identifier: AGPL-3.0-or-later

//! schema.org `VideoObject` in HTML microdata (`itemscope itemtype=".../VideoObject"`).

use std::collections::HashMap;

use async_trait::async_trait;
use flaneur_connector::Result;
use scraper::ElementRef;

use super::schema::{self, VideoObject};
use super::{Resolution, ResolveRequest, Resolver};
use crate::media;

/// The microdata resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct Microdata;

#[async_trait]
impl Resolver for Microdata {
    fn name(&self) -> &'static str {
        "microdata"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        let Some(page) = request.page else {
            return Ok(Resolution::default());
        };
        let objects: Vec<VideoObject> = {
            let html = page.html();
            let scopes = media::selector("[itemscope][itemtype]");
            html.select(&scopes)
                .filter(|el| {
                    el.value()
                        .attr("itemtype")
                        .is_some_and(|t| t.split_whitespace().any(schema::is_video_object))
                })
                .map(|scope| read_video_object(&properties(scope)))
                .collect()
        };
        let mut resolution = Resolution::default();
        for object in objects {
            resolution.merge(object.into_resolution(page));
        }
        Ok(resolution)
    }
}

/// The properties of an item scope, by lowercase name, without those of nested scopes.
fn properties(scope: ElementRef<'_>) -> HashMap<String, Vec<String>> {
    let mut props: HashMap<String, Vec<String>> = HashMap::new();
    let selector = media::selector("[itemprop]");
    for el in scope.select(&selector) {
        let nested = el
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take_while(|a| a.id() != scope.id())
            .any(|a| a.value().attr("itemscope").is_some());
        if nested {
            continue;
        }
        let value = property_value(el);
        for name in el.value().attr("itemprop").unwrap_or("").split_whitespace() {
            props
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(value.clone());
        }
    }
    props
}

/// The value of a property element, as the microdata specification defines it.
fn property_value(el: ElementRef<'_>) -> String {
    let attr = |name| el.value().attr(name).unwrap_or("").trim().to_owned();
    match el.value().name() {
        "meta" => attr("content"),
        "a" | "area" | "link" => attr("href"),
        "img" | "video" | "audio" | "source" | "embed" | "iframe" | "track" => attr("src"),
        "object" => attr("data"),
        "data" | "meter" => attr("value"),
        "time" if el.value().attr("datetime").is_some() => attr("datetime"),
        _ => el.text().collect::<String>().trim().to_owned(),
    }
}

fn read_video_object(props: &HashMap<String, Vec<String>>) -> VideoObject {
    let all = |name: &str| props.get(name).cloned().unwrap_or_default();
    let first = |name: &str| all(name).into_iter().find(|v| !v.is_empty());
    let dimension = |name: &str| first(name).as_deref().and_then(schema::parse_dimension);
    VideoObject {
        content_urls: all("contenturl"),
        encoding_format: first("encodingformat").filter(|f| f.contains('/')),
        name: first("name"),
        description: first("description"),
        duration: first("duration"),
        upload_date: first("uploaddate").or_else(|| first("datepublished")),
        thumbnails: all("thumbnailurl")
            .into_iter()
            .chain(all("thumbnail"))
            .filter(|u| !u.is_empty())
            .map(|u| (u, None, None))
            .collect(),
        width: dimension("width"),
        height: dimension("height"),
        bitrate: first("bitrate"),
        size: first("contentsize"),
        family_friendly: first("isfamilyfriendly"),
        content_rating: first("contentrating"),
    }
}

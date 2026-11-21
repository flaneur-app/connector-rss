// SPDX-License-Identifier: AGPL-3.0-or-later

//! JSON-LD `VideoObject` (`<script type="application/ld+json">`), also inside `@graph`, lists
//! and nested objects (an `Article` whose `video` is a `VideoObject`).

use async_trait::async_trait;
use flaneur_connector::Result;
use serde_json::Value;

use super::schema::{self, VideoObject};
use super::{Resolution, ResolveRequest, Resolver};
use crate::media;

/// The JSON-LD resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonLd;

#[async_trait]
impl Resolver for JsonLd {
    fn name(&self) -> &'static str {
        "json-ld"
    }

    async fn resolve(&self, request: &ResolveRequest<'_>) -> Result<Resolution> {
        let Some(page) = request.page else {
            return Ok(Resolution::default());
        };
        let scripts: Vec<String> = {
            let html = page.html();
            let selector = media::selector(r#"script[type="application/ld+json" i]"#);
            html.select(&selector).map(|s| s.inner_html()).collect()
        };
        let mut resolution = Resolution::default();
        for script in scripts {
            let Ok(value) = serde_json::from_str::<Value>(script.trim()) else {
                tracing::debug!("skipping invalid JSON-LD block");
                continue;
            };
            let mut objects = Vec::new();
            collect_video_objects(&value, &mut objects, 0);
            for object in objects {
                resolution.merge(read_video_object(object).into_resolution(page));
            }
        }
        Ok(resolution)
    }
}

fn collect_video_objects<'a>(value: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect_video_objects(item, out, depth + 1);
            }
        }
        Value::Object(map) => {
            let is_video = match map.get("@type") {
                Some(Value::String(t)) => schema::is_video_object(t),
                Some(Value::Array(types)) => types
                    .iter()
                    .any(|t| t.as_str().is_some_and(schema::is_video_object)),
                _ => false,
            };
            if is_video {
                out.push(value);
            } else {
                for child in map.values() {
                    collect_video_objects(child, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// Strings of a property: a string, a number, a list of them, or objects with a `url` / `@id`
/// / `@value`.
fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Number(n)) => vec![n.to_string()],
        Some(Value::Bool(b)) => vec![b.to_string()],
        Some(Value::Array(items)) => items.iter().flat_map(|i| strings(Some(i))).collect(),
        Some(Value::Object(map)) => ["url", "contentUrl", "@id", "@value", "value", "name"]
            .iter()
            .find_map(|k| map.get(*k))
            .map_or_default(|v| strings(Some(v))),
        _ => Vec::new(),
    }
}

fn first(value: Option<&Value>) -> Option<String> {
    strings(value).into_iter().next()
}

fn dimension(value: Option<&Value>) -> Option<u32> {
    first(value).as_deref().and_then(schema::parse_dimension)
}

fn read_video_object(object: &Value) -> VideoObject {
    let get = |key: &str| object.get(key);
    let thumbnails = match get("thumbnail") {
        Some(Value::Object(_) | Value::Array(_)) => {
            let list = match get("thumbnail") {
                Some(Value::Array(items)) => items.iter().collect(),
                Some(v) => vec![v],
                None => Vec::new(),
            };
            list.into_iter()
                .filter_map(|t| {
                    Some((
                        first(Some(t))?,
                        dimension(t.get("width")),
                        dimension(t.get("height")),
                    ))
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let mut video = VideoObject {
        content_urls: strings(get("contentUrl").or_else(|| get("contentURL"))),
        encoding_format: first(get("encodingFormat")).filter(|f| f.contains('/')),
        name: first(get("name")),
        description: first(get("description")),
        duration: first(get("duration")),
        upload_date: first(get("uploadDate").or_else(|| get("datePublished"))),
        thumbnails,
        width: dimension(get("width")),
        height: dimension(get("height")),
        bitrate: first(get("bitrate")),
        size: first(get("contentSize")),
        family_friendly: first(get("isFamilyFriendly")),
        content_rating: first(get("contentRating")),
    };
    for url in strings(get("thumbnailUrl")) {
        video.thumbnails.push((url, None, None));
    }
    video
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_video_objects_in_graphs() {
        let value: Value = serde_json::from_str(
            r#"{"@context":"https://schema.org","@graph":[
                {"@type":"WebPage","name":"x"},
                {"@type":["VideoObject"],"name":"Clip","contentUrl":"/v.mp4"}]}"#,
        )
        .unwrap();
        let mut found = Vec::new();
        collect_video_objects(&value, &mut found, 0);
        assert_eq!(found.len(), 1);
        assert_eq!(read_video_object(found[0]).content_urls, ["/v.mp4"]);
    }
}

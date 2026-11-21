// SPDX-License-Identifier: AGPL-3.0-or-later

//! Mapping of a schema.org `VideoObject`, shared by the JSON-LD and microdata resolvers.

use flaneur_model::Thumbnail;

use super::{Candidate, PageDocument, Resolution};
use crate::media;

/// The `VideoObject` properties the resolvers read, flattened to strings.
#[derive(Debug, Clone, Default)]
pub(crate) struct VideoObject {
    pub content_urls: Vec<String>,
    pub encoding_format: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub duration: Option<String>,
    pub upload_date: Option<String>,
    pub thumbnails: Vec<(String, Option<u32>, Option<u32>)>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate: Option<String>,
    pub size: Option<String>,
    pub family_friendly: Option<String>,
    pub content_rating: Option<String>,
}

impl VideoObject {
    /// Maps the object into a resolution, resolving URLs against the page.
    pub(crate) fn into_resolution(self, page: &PageDocument) -> Resolution {
        let mut resolution = Resolution::default();
        for href in &self.content_urls {
            let Some(url) = page.resolve(href) else {
                continue;
            };
            let mut candidate = Candidate::new(url, self.encoding_format.clone());
            candidate.width = self.width;
            candidate.height = self.height;
            candidate.bitrate = self.bitrate.as_deref().and_then(parse_bitrate);
            candidate.size = self.size.as_deref().and_then(parse_size);
            resolution.add_candidate(candidate);
        }
        resolution.title = self
            .name
            .map(|n| media::html_to_text(&n))
            .filter(|n| !n.is_empty());
        resolution.description = self
            .description
            .map(|d| media::html_to_text(&d))
            .filter(|d| !d.is_empty());
        resolution.duration = self.duration.as_deref().and_then(media::parse_duration);
        resolution.published = self.upload_date.as_deref().and_then(media::parse_date);
        let thumbnails = self.thumbnails.into_iter().filter_map(|(href, w, h)| {
            Some(Thumbnail {
                url: page.resolve(&href)?,
                width: w,
                height: h,
            })
        });
        media::merge_thumbnails(&mut resolution.thumbnails, thumbnails);
        let family_friendly = self.family_friendly.as_deref().and_then(|v| {
            match v.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "https://schema.org/true" | "http://schema.org/true" => {
                    Some(false)
                }
                "false" | "no" | "https://schema.org/false" | "http://schema.org/false" => {
                    Some(true)
                }
                _ => None,
            }
        });
        let rating = self
            .content_rating
            .as_deref()
            .and_then(media::rating_to_sensitive);
        resolution.sensitive = media::combine_sensitive([family_friendly, rating]);
        resolution
    }
}

/// Whether a schema.org type name is `VideoObject` (`VideoObject`, `schema:VideoObject`,
/// `http://schema.org/VideoObject`...).
pub(crate) fn is_video_object(type_name: &str) -> bool {
    let name = type_name.trim();
    name.rsplit(['/', ':', '#'])
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case("VideoObject"))
}

/// Parses a schema.org dimension: `720`, `"720"`, `"720 px"`.
pub(crate) fn parse_dimension(text: &str) -> Option<u32> {
    text.trim()
        .trim_end_matches("px")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| (1.0..=100_000.0).contains(v))
        .map(|v| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "range checked above"
            )]
            let v = v as u32;
            v
        })
}

/// Parses `bitrate` (`"1200kbps"`, `"1.2 Mbps"`, `"1200000"`) into bit/s.
fn parse_bitrate(text: &str) -> Option<u32> {
    let lower = text.trim().to_ascii_lowercase();
    let (number, factor) = if let Some(n) = lower.strip_suffix("mbps") {
        (n, 1_000_000.0)
    } else if let Some(n) = lower.strip_suffix("kbps") {
        (n, 1_000.0)
    } else {
        (lower.strip_suffix("bps").unwrap_or(&lower), 1.0)
    };
    let value = number.trim().parse::<f64>().ok()? * factor;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range checked"
    )]
    (1.0..f64::from(u32::MAX))
        .contains(&value)
        .then_some(value as u32)
}

/// Parses `contentSize` (`"13952323"`, `"13.9 MB"`) into bytes.
fn parse_size(text: &str) -> Option<u64> {
    let lower = text.trim().to_ascii_lowercase();
    let (number, factor) = [("gb", 1e9), ("mb", 1e6), ("kb", 1e3), ("b", 1.0)]
        .iter()
        .find_map(|(suffix, f)| lower.strip_suffix(suffix).map(|n| (n, *f)))
        .unwrap_or((&lower, 1.0));
    let value = number.trim().parse::<f64>().ok()? * factor;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range checked"
    )]
    (1.0..1e15).contains(&value).then_some(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_video_object_types() {
        assert!(is_video_object("VideoObject"));
        assert!(is_video_object("http://schema.org/VideoObject"));
        assert!(is_video_object("schema:VideoObject"));
        assert!(!is_video_object("ImageObject"));
    }

    #[test]
    fn parses_quantities() {
        assert_eq!(parse_dimension("720"), Some(720));
        assert_eq!(parse_dimension("1280 px"), Some(1280));
        assert_eq!(parse_dimension("0"), None);
        assert_eq!(parse_bitrate("1200kbps"), Some(1_200_000));
        assert_eq!(parse_size("13.9 MB"), Some(13_900_000));
        assert_eq!(parse_size("4060710"), Some(4_060_710));
    }
}

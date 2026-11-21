// SPDX-License-Identifier: AGPL-3.0-or-later

//! Small parsers shared by the feed mapping and the resolvers: MIME types and codecs, ISO 8601
//! durations and dates, content ratings, and HTML to plain text.

use flaneur_model::{AudioTrack, Container, Precision, Published, Seconds, Thumbnail, VideoCodec};
use scraper::{Html, Node};
use url::Url;

/// What a candidate media URL turned out to be, from its MIME type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaClass {
    /// A progressive file (MP4, `WebM`...), played as one muxed stream.
    File,
    /// An HLS playlist.
    Hls,
    /// A DASH manifest.
    Dash,
}

/// Extensions that mark a URL as a video file or a manifest, when no MIME type says so.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "webm", "mov", "mkv", "ogv", "m3u8", "mpd"];

/// The lowercase extension of the last path segment.
fn extension(url: &Url) -> Option<String> {
    url.path()
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// A CSS selector known to be valid.
pub(crate) fn selector(css: &str) -> scraper::Selector {
    scraper::Selector::parse(css).expect("selectors in this crate are valid")
}

/// The MIME essence (`type/subtype`, lowercase, no parameters).
#[must_use]
pub fn essence(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Classifies a MIME type as playable video, or `None` when it is not one.
#[must_use]
pub fn classify_mime(mime: &str) -> Option<MediaClass> {
    let essence = essence(mime);
    match essence.as_str() {
        "application/x-mpegurl" | "application/vnd.apple.mpegurl" | "audio/mpegurl" => {
            Some(MediaClass::Hls)
        }
        "application/dash+xml" => Some(MediaClass::Dash),
        e if e.starts_with("video/") => Some(MediaClass::File),
        _ => None,
    }
}

/// Classifies a URL by its path extension, for candidates without a MIME type.
#[must_use]
pub fn classify_url(url: &Url) -> Option<MediaClass> {
    let ext = extension(url)?;
    if !VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    Some(match ext.as_str() {
        "m3u8" => MediaClass::Hls,
        "mpd" => MediaClass::Dash,
        _ => MediaClass::File,
    })
}

/// Whether a MIME type is a generic binary type that servers use for any file.
#[must_use]
pub fn is_generic_binary(mime: &str) -> bool {
    matches!(
        essence(mime).as_str(),
        "application/octet-stream" | "binary/octet-stream"
    )
}

/// The container of a progressive file, from its MIME type or, failing that, its URL.
#[must_use]
pub fn container(mime: Option<&str>, url: &Url) -> Container {
    let essence = mime.map(essence).filter(|e| !is_generic_binary(e));
    match essence.as_deref() {
        Some("video/mp4" | "video/x-m4v" | "video/quicktime") => Container::Mp4,
        Some("video/webm") => Container::WebM,
        Some("video/mp2t") => Container::Ts,
        Some(other) => Container::Other(other.to_owned()),
        None => match extension(url).as_deref() {
            Some("mp4" | "m4v" | "mov") => Container::Mp4,
            Some("webm") => Container::WebM,
            Some("ts") => Container::Ts,
            _ => Container::Other("application/octet-stream".to_owned()),
        },
    }
}

/// The `codecs` parameter of a MIME type, split into its entries.
#[must_use]
pub fn codecs(mime: &str) -> Vec<String> {
    mime.split(';')
        .skip(1)
        .filter_map(|param| {
            let (name, value) = param.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("codecs")
                .then(|| value.trim().trim_matches('"').to_owned())
        })
        .flat_map(|list| {
            list.split(',')
                .map(|c| c.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .filter(|c| !c.is_empty())
        .collect()
}

/// The video codec among a `codecs` list.
#[must_use]
pub fn video_codec(codecs: &[String]) -> Option<VideoCodec> {
    codecs.iter().find_map(|c| {
        let lower = c.to_ascii_lowercase();
        let family = lower.split('.').next().unwrap_or("");
        match family {
            "avc1" | "avc3" | "h264" => Some(VideoCodec::H264),
            "vp09" | "vp9" => Some(VideoCodec::Vp9),
            "av01" | "av1" => Some(VideoCodec::Av1),
            "hvc1" | "hev1" | "h265" | "hevc" => Some(VideoCodec::H265),
            "vp8" | "vp08" | "theora" | "mp4v" => Some(VideoCodec::Other(c.clone())),
            _ => None,
        }
    })
}

/// The audio track among a `codecs` list, with only its codec known.
#[must_use]
pub fn audio_track(codecs: &[String]) -> Option<AudioTrack> {
    let codec = codecs.iter().find(|c| {
        let lower = c.to_ascii_lowercase();
        let family = lower.split('.').next().unwrap_or("");
        matches!(
            family,
            "mp4a" | "opus" | "vorbis" | "flac" | "ac-3" | "ec-3" | "mp3"
        )
    })?;
    Some(AudioTrack {
        codec: Some(codec.clone()),
        language: None,
        label: None,
        original: None,
        sample_rate: None,
        channels: None,
    })
}

/// Parses an ISO 8601 duration (`PT1H2M3.5S`, `P1DT2H`), rounded down to whole seconds.
///
/// Years and months are rejected: their length in seconds is not defined.
#[must_use]
pub fn parse_iso_duration(text: &str) -> Option<Seconds> {
    let text = text.trim();
    let rest = text.strip_prefix('P').or_else(|| text.strip_prefix('p'))?;
    let mut total = 0f64;
    let mut in_time = false;
    let mut number = String::new();
    let mut any = false;
    for c in rest.chars() {
        match c.to_ascii_uppercase() {
            'T' => in_time = true,
            d if d.is_ascii_digit() || d == '.' || d == ',' => {
                number.push(if d == ',' { '.' } else { d });
            }
            unit => {
                let value: f64 = number.parse().ok()?;
                number.clear();
                any = true;
                total += value
                    * match (unit, in_time) {
                        ('W', false) => 604_800.0,
                        ('D', false) => 86_400.0,
                        ('H', true) => 3_600.0,
                        ('M', true) => 60.0,
                        ('S', true) => 1.0,
                        _ => return None,
                    };
            }
        }
    }
    if !number.is_empty() || !any || !(0.0..=f64::from(u32::MAX)).contains(&total) {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "range checked above; rounding down to whole seconds is intended"
    )]
    Some(total as Seconds)
}

/// Parses a duration in whole seconds (`og:video:duration`), or an ISO 8601 duration.
#[must_use]
pub fn parse_duration(text: &str) -> Option<Seconds> {
    let text = text.trim();
    text.parse::<Seconds>()
        .ok()
        .or_else(|| parse_iso_duration(text))
}

/// Parses an ISO 8601 / RFC 3339 or RFC 2822 date-time (`Exact`), or a bare `YYYY-MM-DD`
/// date (`Day`, at midnight UTC).
#[must_use]
pub fn parse_date(text: &str) -> Option<Published> {
    let text = text.trim();
    let exact = |at: jiff::Timestamp| Published {
        at: at.as_second(),
        precision: Precision::Exact,
    };
    if let Ok(at) = text.parse::<jiff::Timestamp>() {
        return Some(exact(at));
    }
    if let Ok(zoned) = jiff::fmt::rfc2822::parse(text) {
        return Some(exact(zoned.timestamp()));
    }
    let day = text.get(..10).unwrap_or(text);
    let date = day.parse::<jiff::civil::Date>().ok()?;
    let at = jiff::tz::TimeZone::UTC
        .to_timestamp(date.at(0, 0, 0, 0))
        .ok()?;
    Some(Published {
        at: at.as_second(),
        precision: Precision::Day,
    })
}

/// Maps a content rating to the `sensitive` flag.
///
/// Understands `<meta name="rating">` values (`general`, `adult`, `mature`, the RTA label),
/// Media RSS `urn:simple` (`adult` / `nonadult`), MPAA and V-chip ratings, and iTunes
/// `explicit` values. Anything else is unknown (`None`), never "safe".
#[must_use]
pub fn rating_to_sensitive(rating: &str) -> Option<bool> {
    let value = rating.trim().to_ascii_lowercase();
    if value.contains("rta-5042-1996-1400-1577-rta") {
        return Some(true);
    }
    match value.as_str() {
        "adult" | "mature" | "restricted" | "explicit" | "nsfw" | "18+" | "x" | "xxx" | "r"
        | "nc-17" | "tv-ma" | "yes" | "true" | "porn" | "pornographic" => Some(true),
        "general" | "nonadult" | "safe for kids" | "safe" | "g" | "pg" | "tv-y" | "tv-y7"
        | "tv-g" | "clean" | "no" | "false" | "family" | "kids" | "14 years" => Some(false),
        _ => None,
    }
}

/// Folds sensitivity markers from several sources: any `true` wins, then any `false`.
#[must_use]
pub fn combine_sensitive(values: impl IntoIterator<Item = Option<bool>>) -> Option<bool> {
    let mut result = None;
    for value in values.into_iter().flatten() {
        if value {
            return Some(true);
        }
        result = Some(false);
    }
    result
}

/// Converts an HTML fragment into plain text: tags dropped, entities decoded, `<br>` and block
/// ends turned into line breaks, runs of blank lines collapsed.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    if !html.contains('<') && !html.contains('&') {
        return html.trim().to_owned();
    }
    let fragment = Html::parse_fragment(html);
    let mut out = String::new();
    for edge in fragment.root_element().traverse() {
        match edge {
            ego_tree::iter::Edge::Open(node) => match node.value() {
                Node::Text(text) => out.push_str(text),
                Node::Element(el) if el.name() == "br" => out.push('\n'),
                _ => {}
            },
            ego_tree::iter::Edge::Close(node) => {
                if let Node::Element(el) = node.value()
                    && matches!(
                        el.name(),
                        "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr"
                    )
                {
                    out.push('\n');
                }
            }
        }
    }
    let mut text = String::new();
    let mut blank = 0;
    for line in out.lines().map(str::trim) {
        if line.is_empty() {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        text.push_str(line);
    }
    text
}

/// The first `<img>` of an HTML fragment, with its `width` and `height` attributes.
#[must_use]
pub fn first_image(html: &str, base: Option<&Url>) -> Option<Thumbnail> {
    if !html.contains("<img") {
        return None;
    }
    let fragment = Html::parse_fragment(html);
    let selector = scraper::Selector::parse("img[src]").ok()?;
    let img = fragment.select(&selector).next()?;
    let url = resolve_url(base, img.value().attr("src")?)?;
    let dim = |name| img.value().attr(name).and_then(|v| v.trim().parse().ok());
    Some(Thumbnail {
        url,
        width: dim("width"),
        height: dim("height"),
    })
}

/// Resolves a possibly relative URL against a base, keeping only `http(s)` results.
#[must_use]
pub fn resolve_url(base: Option<&Url>, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let url = match base {
        Some(base) => base.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then_some(url)
}

/// Adds thumbnails to a list, merging entries with the same URL (known sizes fill unknown ones).
pub fn merge_thumbnails(into: &mut Vec<Thumbnail>, from: impl IntoIterator<Item = Thumbnail>) {
    for thumb in from {
        if let Some(existing) = into.iter_mut().find(|t| t.url == thumb.url) {
            existing.width = existing.width.or(thumb.width);
            existing.height = existing.height.or(thumb.height);
        } else {
            into.push(thumb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_durations() {
        assert_eq!(parse_iso_duration("PT20S"), Some(20));
        assert_eq!(parse_iso_duration("PT1H2M3.9S"), Some(3723));
        assert_eq!(parse_iso_duration("P1DT1S"), Some(86_401));
        assert_eq!(parse_iso_duration("PT"), None);
        assert_eq!(parse_iso_duration("P1M"), None);
        assert_eq!(parse_iso_duration("20"), None);
        assert_eq!(parse_duration("95"), Some(95));
    }

    #[test]
    fn parses_dates_with_precision() {
        let exact = parse_date("2026-11-20T11:48:41+01:00").unwrap();
        assert_eq!(exact.at, 1_795_171_721);
        assert_eq!(exact.precision, Precision::Exact);
        let day = parse_date("2026-11-20").unwrap();
        assert_eq!(day.precision, Precision::Day);
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn maps_codecs() {
        let list = codecs("video/mp4; codecs=\"avc1.42E01E, mp4a.40.2\"");
        assert_eq!(list, ["avc1.42E01E", "mp4a.40.2"]);
        assert_eq!(video_codec(&list), Some(VideoCodec::H264));
        assert_eq!(
            audio_track(&list).unwrap().codec.as_deref(),
            Some("mp4a.40.2")
        );
        let vp9 = codecs("video/mp4; codecs=vp09, mp4a.40.2");
        assert_eq!(video_codec(&vp9), Some(VideoCodec::Vp9));
        assert_eq!(codecs("video/mp4"), Vec::<String>::new());
    }

    #[test]
    fn classifies_media() {
        assert_eq!(classify_mime("video/mp4"), Some(MediaClass::File));
        assert_eq!(
            classify_mime("application/vnd.apple.mpegurl"),
            Some(MediaClass::Hls)
        );
        assert_eq!(classify_mime("text/html; charset=utf-8"), None);
        let url = Url::parse("https://example.org/a/b.M3U8?x=1").unwrap();
        assert_eq!(classify_url(&url), Some(MediaClass::Hls));
        let page = Url::parse("https://example.org/video/x.html").unwrap();
        assert_eq!(classify_url(&page), None);
    }

    #[test]
    fn maps_ratings() {
        assert_eq!(rating_to_sensitive("general"), Some(false));
        assert_eq!(rating_to_sensitive("Adult"), Some(true));
        assert_eq!(rating_to_sensitive("mature"), Some(true));
        assert_eq!(
            rating_to_sensitive("RTA-5042-1996-1400-1577-RTA"),
            Some(true)
        );
        assert_eq!(rating_to_sensitive("nonadult"), Some(false));
        assert_eq!(rating_to_sensitive("something"), None);
        assert_eq!(
            combine_sensitive([None, Some(false), Some(true)]),
            Some(true)
        );
        assert_eq!(combine_sensitive([None, Some(false)]), Some(false));
        assert_eq!(combine_sensitive([None, None]), None);
    }

    #[test]
    fn converts_html_to_text() {
        let html = "Lors d'une course... La suite<br><a href=\"x\"><img src=\"t.jpg\"></a>\
                    <p>Ce media <a href=\"y\">X</a> est apparu.</p>";
        assert_eq!(
            html_to_text(html),
            "Lors d'une course... La suite\nCe media X est apparu."
        );
        assert_eq!(html_to_text("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(html_to_text("plain"), "plain");
    }

    #[test]
    fn finds_first_image_with_size() {
        let base = Url::parse("https://example.org/feed").unwrap();
        let thumb = first_image(
            "<a><img width=\"640\" height=\"360\" src=\"/t.jpg\"></a>",
            Some(&base),
        )
        .unwrap();
        assert_eq!(thumb.url.as_str(), "https://example.org/t.jpg");
        assert_eq!((thumb.width, thumb.height), (Some(640), Some(360)));
    }
}

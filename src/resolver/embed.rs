// SPDX-License-Identifier: AGPL-3.0-or-later

//! Players of known video hosts embedded in the page, for `Playable::handoff`.
//!
//! The raw page source is scanned, not only the DOM, so a player that a script writes into the
//! page (Koreus builds its YouTube or Dailymotion iframe in a JavaScript function) counts too.
//! The chain uses the handoff only when none of the page's own files plays: a self-hosted file
//! always wins.
//!
//! | Host | Recognized | Item id |
//! | --- | --- | --- |
//! | YouTube (`youtube`) | `youtube.com/embed/<id>`, `youtube-nocookie.com/embed/<id>` | the 11-character video id |
//! | Dailymotion (`dailymotion`) | `dailymotion.com/embed/video/<id>`, `geo.dailymotion.com/player/<player>.html?video=<id>`, `dai.ly/<id>`, `dailymotion.com/video/<id>` | the video id (`x8abc12`) |
//! | Odysee (`odysee`) | `odysee.com/$/embed/...` | what connector-odysee's `parse_url` gives for the same link: the full claim id when the URL has it, else the LBRY URL without its scheme (`@channel#1/name#a`) |
//!
//! PeerTube embeds (`/videos/embed/<uuid>`) are not recognized: they live on any host, and no
//! connector defines their ids yet.

use async_trait::async_trait;
use flaneur_connector::Result;
use flaneur_model::{ItemRef, ServiceId};
use percent_encoding::percent_decode_str;
use url::Url;

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

/// A known host: a text marker to look for, and how to read an item from a URL around it.
struct Host {
    marker: &'static str,
    read: fn(&Url) -> Option<ItemRef>,
}

const HOSTS: &[Host] = &[
    Host {
        marker: "youtube.com/embed/",
        read: youtube,
    },
    Host {
        marker: "youtube-nocookie.com/embed/",
        read: youtube,
    },
    Host {
        marker: "dailymotion.com/embed/video/",
        read: dailymotion,
    },
    Host {
        marker: "geo.dailymotion.com/player",
        read: dailymotion,
    },
    Host {
        marker: "dailymotion.com/video/",
        read: dailymotion,
    },
    Host {
        marker: "dai.ly/",
        read: dailymotion,
    },
    Host {
        marker: "odysee.com/$/embed/",
        read: odysee,
    },
];

/// The first video of a known host embedded or linked in an HTML (or script) source.
#[must_use]
pub fn handoff(source: &str) -> Option<ItemRef> {
    all(source).into_iter().next()
}

/// Every video of a known host embedded or linked in a source, in source order, without
/// duplicates.
#[must_use]
pub fn all(source: &str) -> Vec<ItemRef> {
    let mut found: Vec<(usize, ItemRef)> = Vec::new();
    for host in HOSTS {
        for (at, _) in source.match_indices(host.marker) {
            let Some((start, url)) = url_around(source, at) else {
                continue;
            };
            if let Some(item) = (host.read)(&url) {
                match found.iter_mut().find(|(_, i)| *i == item) {
                    Some(earlier) => earlier.0 = earlier.0.min(start),
                    None => found.push((start, item)),
                }
            }
        }
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, item)| item).collect()
}

/// The URL whose host ends right before `at`: the host is read backwards, the rest forwards up
/// to the first character that cannot be in an attribute value or a script string.
fn url_around(source: &str, at: usize) -> Option<(usize, Url)> {
    let before = &source[..at];
    let host_len = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .map(char::len_utf8)
        .sum::<usize>();
    let start = at - host_len;
    let rest = &source[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | ')' | '`'))
        .unwrap_or(rest.len());
    let text = rest[..end].replace("&amp;", "&");
    Url::parse(&format!("https://{text}"))
        .ok()
        .map(|url| (start, url))
}

fn host_is(url: &Url, domain: &str) -> bool {
    url.host_str()
        .is_some_and(|h| h == domain || h.ends_with(&format!(".{domain}")))
}

fn segments(url: &Url) -> Vec<&str> {
    url.path_segments()
        .map_or_default(|s| s.filter(|s| !s.is_empty()).collect())
}

fn youtube(url: &Url) -> Option<ItemRef> {
    if !host_is(url, "youtube.com") && !host_is(url, "youtube-nocookie.com") {
        return None;
    }
    match segments(url).as_slice() {
        ["embed", id, ..] if is_youtube_id(id) => {
            Some(ItemRef::new(ServiceId::new("youtube"), *id))
        }
        _ => None,
    }
}

fn is_youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn dailymotion(url: &Url) -> Option<ItemRef> {
    let id = if host_is(url, "dai.ly") {
        segments(url).first().copied().map(str::to_owned)
    } else if url.host_str() == Some("geo.dailymotion.com") {
        url.query_pairs()
            .find(|(k, _)| k == "video")
            .map(|(_, v)| v.into_owned())
    } else if host_is(url, "dailymotion.com") {
        match segments(url).as_slice() {
            ["embed", "video", id, ..] | ["video", id, ..] => Some((*id).to_owned()),
            _ => None,
        }
    } else {
        None
    }?;
    // Old page URLs append a slug: `/video/x7tgad0_some-title`.
    let id = id.split('_').next().unwrap_or("");
    let valid = (5..=12).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric());
    valid.then(|| ItemRef::new(ServiceId::new("dailymotion"), id))
}

/// `odysee.com/$/embed/...`, read the way connector-odysee's `parse_url` reads it.
fn odysee(url: &Url) -> Option<ItemRef> {
    if !host_is(url, "odysee.com") {
        return None;
    }
    let item = |id: String| Some(ItemRef::new(ServiceId::new("odysee"), id));
    let raw = segments(url);
    let ["$", "embed", rest @ ..] = raw.as_slice() else {
        return None;
    };
    match rest {
        // The old form: `$/embed/<name>/<claim id>`.
        [_name, id] if is_claim_id(id) => item((*id).to_owned()),
        [stream] => {
            let stream = OdyseeSegment::parse(stream)?;
            if stream.is_channel() {
                return None;
            }
            item(
                stream
                    .full_id()
                    .map_or_else(|| stream.lbry(), str::to_owned),
            )
        }
        [channel, stream] => {
            let channel = OdyseeSegment::parse(channel)?;
            let stream = OdyseeSegment::parse(stream)?;
            if !channel.is_channel() || stream.is_channel() {
                return None;
            }
            item(stream.full_id().map_or_else(
                || format!("{}/{}", channel.lbry(), stream.lbry()),
                str::to_owned,
            ))
        }
        _ => None,
    }
}

/// A full LBRY claim id: 40 lowercase hexadecimal characters.
fn is_claim_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One `name[:modifier]` segment of an Odysee URL.
struct OdyseeSegment {
    name: String,
    modifier: Option<String>,
}

impl OdyseeSegment {
    fn parse(raw: &str) -> Option<Self> {
        let decoded = percent_decode_str(raw).decode_utf8().ok()?;
        let (name, modifier) = match decoded.split_once([':', '#']) {
            Some((name, modifier)) => (name, Some(modifier.to_owned())),
            None => (decoded.as_ref(), None),
        };
        if name.is_empty() || name == "@" {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            modifier: modifier.filter(|m| !m.is_empty()),
        })
    }

    fn is_channel(&self) -> bool {
        self.name.starts_with('@')
    }

    fn full_id(&self) -> Option<&str> {
        self.modifier.as_deref().filter(|m| is_claim_id(m))
    }

    fn lbry(&self) -> String {
        match &self.modifier {
            Some(m) => format!("{}#{m}", self.name),
            None => self.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str) -> Option<String> {
        handoff(source).map(|i| i.to_string())
    }

    #[test]
    fn finds_youtube_embeds() {
        let src = r#"x.innerHTML='<iframe src="//www.youtube.com/embed/hH7HeBpPbQA?rel=0">';"#;
        assert_eq!(found(src).as_deref(), Some("youtube:hH7HeBpPbQA"));
        let nocookie = r#"<iframe src="https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ">"#;
        assert_eq!(found(nocookie).as_deref(), Some("youtube:dQw4w9WgXcQ"));
        assert_eq!(found(r#"<a href="https://youtube.com/embed/short">"#), None);
        assert_eq!(found("<video src=a.mp4>"), None);
    }

    #[test]
    fn finds_dailymotion_players_and_links() {
        let cases = [
            (
                r#"<iframe src="https://www.dailymotion.com/embed/video/x8abc12?autoplay=1">"#,
                "x8abc12",
            ),
            (
                r#"'<iframe src="https://geo.dailymotion.com/player/xak7i.html?video=xby943q">'"#,
                "xby943q",
            ),
            (
                r#"<iframe src="https://geo.dailymotion.com/player.html?mute=true&amp;video=x9zz111">"#,
                "x9zz111",
            ),
            (r#"<a href="https://dai.ly/x7tgad0">watch</a>"#, "x7tgad0"),
            (
                r#"<a href="http://www.dailymotion.com/video/x7tgad0_a-title">"#,
                "x7tgad0",
            ),
        ];
        for (source, id) in cases {
            assert_eq!(found(source), Some(format!("dailymotion:{id}")), "{source}");
        }
        // A channel link is not a video.
        assert_eq!(
            found(r#"<a href="https://www.dailymotion.com/koreus">"#),
            None
        );
    }

    #[test]
    fn reads_odysee_embeds_like_connector_odysee() {
        let id = "ee58d95e7e42ec9479e13b28d57556f9ffb373dc";
        let cases = [
            (
                format!("https://odysee.com/$/embed/roundup/{id}"),
                id.to_owned(),
            ),
            (
                format!("https://odysee.com/$/embed/roundup:{id}?r=x"),
                id.to_owned(),
            ),
            (
                "https://odysee.com/$/embed/@Odysee:8/channel-roundup:e".to_owned(),
                "@Odysee#8/channel-roundup#e".to_owned(),
            ),
            (
                "https://odysee.com/$/embed/missinfo".to_owned(),
                "missinfo".to_owned(),
            ),
        ];
        for (url, expected) in cases {
            let source = format!(r#"<iframe id="lbry-iframe" src="{url}" allowfullscreen>"#);
            assert_eq!(found(&source), Some(format!("odysee:{expected}")), "{url}");
        }
        assert_eq!(
            found(r#"<iframe src="https://odysee.com/$/embed/@Odysee:8">"#),
            None
        );
    }

    #[test]
    fn lists_every_embed_in_source_order() {
        let source = r#"<a href="https://dai.ly/x7tgad0"></a>
            <iframe src="https://www.youtube.com/embed/dQw4w9WgXcQ"></iframe>
            <a href="https://dai.ly/x7tgad0"></a>"#;
        let ids: Vec<_> = all(source).iter().map(ToString::to_string).collect();
        assert_eq!(ids, ["dailymotion:x7tgad0", "youtube:dQw4w9WgXcQ"]);
    }
}

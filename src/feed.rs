// SPDX-License-Identifier: AGPL-3.0-or-later

//! Parsing RSS / Atom / JSON Feed documents (with `feed-rs`) and mapping them into the model.

use flaneur_connector::{Error, Result};
use flaneur_model::{
    Channel, ChannelRef, ChannelSummary, ContentFlags, Item, ItemKind, ItemRef, Precision,
    Published, ReactionCounts, Seconds, Thumbnail,
};
use url::Url;

use crate::id::ItemId;
use crate::media;
use crate::resolver::FeedMedia;
use crate::service_id;

/// A parsed feed: its header and its video items, newest first.
#[derive(Debug, Clone)]
pub struct ParsedFeed {
    /// The channel page header.
    pub channel: Channel,
    /// Video items, newest first.
    pub entries: Vec<FeedEntry>,
}

/// One video item of a feed.
#[derive(Debug, Clone)]
pub struct FeedEntry {
    /// Its decoded id.
    pub id: ItemId,
    /// The item, as listings show it.
    pub item: Item,
    /// The media the feed attached to it.
    pub media: Vec<FeedMedia>,
}

impl ParsedFeed {
    /// The entry with this `key` (see [`ItemId`]).
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&FeedEntry> {
        self.entries.iter().find(|e| e.id.key == key)
    }
}

/// Parses a feed document fetched from `feed_url` (relative links resolve against it).
///
/// Items are kept when the feed attaches a video to them, or when they attach no media at all
/// but link to a page (a resolver may find a video there). Items whose only media are audio or
/// images (podcasts, photo feeds) are left out.
pub fn parse(feed_url: &Url, channel_id: &str, body: &[u8]) -> Result<ParsedFeed> {
    // A missing `guid` stays empty instead of feed-rs's hash of link and title, so the id
    // falls back to the link, which can be resolved.
    let parser = feed_rs::parser::Builder::new()
        .base_uri(Some(feed_url.as_str()))
        .id_generator(|_, _, _| String::new())
        .build();
    let feed = parser
        .parse(body)
        .map_err(|e| Error::Parse(format!("not an RSS, Atom or JSON feed: {e}")))?;

    let channel_ref = ChannelRef::new(service_id(), channel_id);
    let name = feed
        .title
        .as_ref()
        .map(|t| text(&t.content, t.content_type.subty().as_str()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| feed_url.host_str().unwrap_or(feed_url.as_str()).to_owned());
    let avatars: Vec<Thumbnail> = [feed.logo.as_ref(), feed.icon.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|image| image_thumbnail(feed_url, image))
        .collect();
    let summary = ChannelSummary {
        r#ref: channel_ref,
        name,
        avatars,
        verified: None,
    };
    let feed_sensitive = feed
        .rating
        .as_ref()
        .and_then(|r| media::rating_to_sensitive(&r.value));

    let mut entries: Vec<FeedEntry> = feed
        .entries
        .iter()
        .filter_map(|entry| map_entry(feed_url, &summary, feed_sensitive, entry))
        .collect();
    // Newest first; undated items keep their feed order, after the dated ones.
    entries.sort_by_key(|e| std::cmp::Reverse(e.item.published.map(|p| p.at)));

    let channel = Channel {
        handle: None,
        // Channel descriptions are HTML in RSS whatever the declared type.
        description: feed
            .description
            .as_ref()
            .map(|d| media::html_to_text(&d.content))
            .filter(|d| !d.is_empty()),
        subscribers: None,
        banners: Vec::new(),
        subscribed: None,
        web_url: feed
            .links
            .iter()
            .filter(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
            .find(|l| l.media_type.as_deref().is_none_or(|m| m.contains("html")))
            .and_then(|l| media::resolve_url(Some(feed_url), &l.href)),
        summary,
    };
    Ok(ParsedFeed { channel, entries })
}

fn map_entry(
    feed_url: &Url,
    channel: &ChannelSummary,
    feed_sensitive: Option<bool>,
    entry: &feed_rs::model::Entry,
) -> Option<FeedEntry> {
    let page = entry
        .links
        .iter()
        .filter(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .find(|l| {
            l.media_type
                .as_deref()
                .is_none_or(|m| media::classify_mime(m).is_none())
        })
        .and_then(|l| media::resolve_url(Some(feed_url), &l.href));

    let EntryMedia {
        media: feed_media,
        mut thumbnails,
        ratings,
        duration,
    } = entry_media(feed_url, entry);

    let has_video = feed_media.iter().any(FeedMedia::is_video);
    if !has_video && (!feed_media.is_empty() || page.is_none()) {
        return None;
    }

    let summary_html = entry.summary.as_ref().map(|s| {
        (
            s.content.as_str(),
            s.content_type.subty().as_str().to_owned(),
        )
    });
    let content_html = entry.content.as_ref().and_then(|c| {
        c.body
            .as_deref()
            .map(|b| (b, c.content_type.subty().as_str().to_owned()))
    });
    for (html, _) in summary_html.iter().chain(content_html.iter()) {
        if let Some(img) = media::first_image(html, page.as_ref().or(Some(feed_url))) {
            media::merge_thumbnails(&mut thumbnails, [img]);
        }
    }
    let description = summary_html
        .or(content_html)
        // RSS descriptions are HTML whatever the declared type.
        .map(|(body, _)| media::html_to_text(body))
        .or_else(|| {
            entry
                .media
                .iter()
                .find_map(|m| m.description.as_ref())
                .map(|d| text(&d.content, d.content_type.subty().as_str()))
        })
        .filter(|d| !d.is_empty());

    let video_url = feed_media
        .iter()
        .find(|m| m.is_video())
        .map(|m| m.url.clone());
    let guid = Some(entry.id.as_str()).filter(|g| !g.is_empty());
    let id = ItemId::new(Some(feed_url.clone()), guid, page.clone(), video_url)?;

    let title = entry
        .title
        .as_ref()
        .map(|t| text(&t.content, t.content_type.subty().as_str()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| page.as_ref().map_or_else(|| id.key.clone(), Url::to_string));

    let item = Item {
        r#ref: ItemRef::new(service_id(), id.encode()),
        kind: ItemKind::Video,
        title,
        description,
        duration,
        published: entry.published.or(entry.updated).map(|at| Published {
            at: at.timestamp(),
            precision: Precision::Exact,
        }),
        scheduled_start: None,
        channel: Some(channel.clone()),
        thumbnails,
        views: entry
            .media
            .iter()
            .find_map(|m| m.community.as_ref()?.stats_views),
        reactions: ReactionCounts::default(),
        flags: ContentFlags {
            sensitive: media::combine_sensitive(ratings.into_iter().chain([feed_sensitive])),
            age_restricted: None,
            members_only: None,
        },
        web_url: page,
        watch: None,
    };
    Some(FeedEntry {
        id,
        item,
        media: feed_media,
    })
}

/// The media an entry attaches, with their thumbnails, ratings and the video's duration.
struct EntryMedia {
    media: Vec<FeedMedia>,
    thumbnails: Vec<Thumbnail>,
    ratings: Vec<Option<bool>>,
    duration: Option<Seconds>,
}

fn entry_media(feed_url: &Url, entry: &feed_rs::model::Entry) -> EntryMedia {
    let mut feed_media: Vec<FeedMedia> = Vec::new();
    let mut thumbnails: Vec<Thumbnail> = Vec::new();
    let mut ratings = Vec::new();
    let mut duration = None;
    for object in &entry.media {
        for content in &object.content {
            let Some(url) = content.url.clone() else {
                continue;
            };
            ratings.push(
                content
                    .rating
                    .as_ref()
                    .and_then(|r| media::rating_to_sensitive(&r.value)),
            );
            let media = FeedMedia {
                url,
                mime: content.content_type.as_ref().map(ToString::to_string),
                width: content.width,
                height: content.height,
                size: content.size,
                duration: content.duration.or(object.duration).map(seconds),
            };
            if media.is_video() {
                duration = duration.or(media.duration);
            }
            if !feed_media.iter().any(|m| m.url == media.url) {
                feed_media.push(media);
            }
        }
        let media_thumbs = object
            .thumbnails
            .iter()
            .filter_map(|t| image_thumbnail(feed_url, &t.image));
        media::merge_thumbnails(&mut thumbnails, media_thumbs);
    }
    for link in entry
        .links
        .iter()
        .filter(|l| l.rel.as_deref() == Some("enclosure"))
    {
        if let Some(url) = media::resolve_url(Some(feed_url), &link.href)
            && !feed_media.iter().any(|m| m.url == url)
        {
            feed_media.push(FeedMedia {
                mime: link.media_type.clone(),
                size: link.length,
                ..FeedMedia::url(url)
            });
        }
    }

    EntryMedia {
        media: feed_media,
        thumbnails,
        ratings,
        duration,
    }
}

/// Text of a feed text construct, as plain text.
fn text(content: &str, subtype: &str) -> String {
    if subtype.contains("html") {
        media::html_to_text(content)
    } else {
        content.trim().to_owned()
    }
}

fn image_thumbnail(base: &Url, image: &feed_rs::model::Image) -> Option<Thumbnail> {
    Some(Thumbnail {
        url: media::resolve_url(Some(base), &image.uri)?,
        width: image.width,
        height: image.height,
    })
}

fn seconds(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}

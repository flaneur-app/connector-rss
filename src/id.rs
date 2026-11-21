// SPDX-License-Identifier: AGPL-3.0-or-later

//! The encoding of [`ItemRef::id`](flaneur_model::ItemRef) for feed items.
//!
//! A feed item has no id a server could look up later, so the id carries what is needed to find
//! the item again and to play it. It is a `application/x-www-form-urlencoded` string with these
//! keys, always in this order, each at most once:
//!
//! | Key | Value | When |
//! | --- | --- | --- |
//! | `feed` | URL of the feed the item comes from (the [`ChannelRef`](flaneur_model::ChannelRef) id) | when known |
//! | `key` | identity of the item in that feed: its `guid` / Atom `id`, else its link, else its media URL | always |
//! | `page` | the item's page (`<link>`) | when it differs from `key` |
//! | `media` | the item's video URL (enclosure, `media:content`) | only when there is no `page` |
//!
//! For example, a Koreus item whose `guid` is its link encodes as
//! `feed=https%3A%2F%2Ffeeds.feedburner.com%2FKoreus-videos&key=https%3A%2F%2Fwww.koreus.com%2Fvideo%2Fillusion-tandem.html`.
//!
//! The id is stable as long as the feed URL and the item's `guid` (or link) do not change, which
//! is what the local history and subscriptions need. Resolving re-reads the feed to find the item
//! by `key` (title, date, enclosures); when the item has left the feed, `page` (or `key` when it
//! is a URL) or `media` is enough to resolve it from the page alone.

use url::{Url, form_urlencoded};

/// A decoded feed item id. See the [module documentation](self) for the encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemId {
    /// The feed the item comes from.
    pub feed: Option<Url>,
    /// Identity of the item in its feed: `guid`, else link, else media URL.
    pub key: String,
    /// The item's page, when it has one.
    pub page: Option<Url>,
    /// The item's video URL, kept only for items without a page.
    pub media: Option<Url>,
}

impl ItemId {
    /// Builds the id of a feed item from what the feed gives.
    ///
    /// Returns `None` when the item has neither a guid, a link nor a media URL: nothing could
    /// find or play it again.
    #[must_use]
    pub fn new(
        feed: Option<Url>,
        guid: Option<&str>,
        page: Option<Url>,
        media: Option<Url>,
    ) -> Option<Self> {
        let guid = guid.map(str::trim).filter(|g| !g.is_empty());
        let key = match (guid, &page, &media) {
            (Some(guid), _, _) => guid.to_owned(),
            (None, Some(page), _) => page.to_string(),
            (None, None, Some(media)) => media.to_string(),
            (None, None, None) => return None,
        };
        let media = if page.is_some() { None } else { media };
        Some(Self {
            feed,
            key,
            page,
            media,
        })
    }

    /// The encoded form, used as `ItemRef::id`.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = form_urlencoded::Serializer::new(String::new());
        if let Some(feed) = &self.feed {
            out.append_pair("feed", feed.as_str());
        }
        out.append_pair("key", &self.key);
        if let Some(page) = self.page.as_ref().filter(|p| p.as_str() != self.key) {
            out.append_pair("page", page.as_str());
        }
        if let Some(media) = self.media.as_ref().filter(|_| self.page.is_none()) {
            out.append_pair("media", media.as_str());
        }
        out.finish()
    }

    /// Decodes an `ItemRef::id`; `None` when it was not produced by [`ItemId::encode`].
    #[must_use]
    pub fn decode(id: &str) -> Option<Self> {
        let mut feed = None;
        let mut key = None;
        let mut page = None;
        let mut media = None;
        for (name, value) in form_urlencoded::parse(id.as_bytes()) {
            match name.as_ref() {
                "feed" => feed = Some(Url::parse(&value).ok()?),
                "key" => key = Some(value.into_owned()),
                "page" => page = Some(Url::parse(&value).ok()?),
                "media" => media = Some(Url::parse(&value).ok()?),
                _ => return None,
            }
        }
        let key = key?;
        // `page` is omitted when it equals `key`.
        let page = page.or_else(|| {
            media
                .is_none()
                .then(|| Url::parse(&key).ok())
                .flatten()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
        });
        Some(Self {
            feed,
            key,
            page,
            media,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn guid_equal_to_link_is_stored_once() {
        let page = url("https://www.koreus.com/video/illusion-tandem.html");
        let id = ItemId::new(
            Some(url("https://feeds.feedburner.com/Koreus-videos")),
            Some(page.as_str()),
            Some(page.clone()),
            Some(url(
                "https://embed.koreus.com/00071/202611/illusion-tandem.mp4",
            )),
        )
        .unwrap();
        let encoded = id.encode();
        assert_eq!(
            encoded,
            "feed=https%3A%2F%2Ffeeds.feedburner.com%2FKoreus-videos\
             &key=https%3A%2F%2Fwww.koreus.com%2Fvideo%2Fillusion-tandem.html"
        );
        let back = ItemId::decode(&encoded).unwrap();
        assert_eq!(back.page, Some(page));
        assert_eq!(back.media, None);
    }

    #[test]
    fn opaque_guid_keeps_the_page() {
        let id = ItemId::new(
            Some(url("https://example.org/feed.xml")),
            Some("tag:example.org,2026:42"),
            Some(url("https://example.org/videos/42")),
            None,
        )
        .unwrap();
        let back = ItemId::decode(&id.encode()).unwrap();
        assert_eq!(back, id);
        assert_eq!(back.page.unwrap().as_str(), "https://example.org/videos/42");
    }

    #[test]
    fn item_without_page_keeps_its_media() {
        let media = url("https://cdn.example.org/1.mp4");
        let id = ItemId::new(None, Some("1"), None, Some(media.clone())).unwrap();
        let back = ItemId::decode(&id.encode()).unwrap();
        assert_eq!(back.media, Some(media));
        assert_eq!(back.page, None);
        assert_eq!(back.feed, None);
    }

    #[test]
    fn rejects_foreign_ids_and_empty_items() {
        assert_eq!(ItemId::decode("dQw4w9WgXcQ"), None);
        assert_eq!(ItemId::decode("key=a&bogus=b"), None);
        assert_eq!(ItemId::new(None, Some("  "), None, None), None);
    }
}

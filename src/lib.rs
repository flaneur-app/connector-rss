// SPDX-License-Identifier: AGPL-3.0-or-later

//! The RSS & Atom connector of Flâneur.
//!
//! - **A feed is a channel.** [`ChannelRef`](flaneur_model::ChannelRef) ids are feed URLs;
//!   [`Channels::channel`](flaneur_connector::Channels::channel) reads the feed header and
//!   [`Channels::items`](flaneur_connector::Channels::items) its video items, newest first.
//! - **Items are found again by their id**, which encodes the feed URL and the item's guid or
//!   link (see [`id`]).
//! - **Playing an item runs the [resolver chain](resolver)** on its enclosures and its page,
//!   and keeps only the media URLs that a ranged request confirms are video.
//! - **Following feeds and the history are local** (`Support::Local`): the core keeps them, and
//!   builds the subscription feed by merging the items of every followed feed by date. There
//!   are no accounts.
//!
//! ```no_run
//! # async fn demo() -> flaneur_connector::Result<()> {
//! use flaneur_connector::{ChannelTab, Connector, Target};
//! use flaneur_connector_rss::RssConnector;
//!
//! let rss = RssConnector::new()?;
//! let url = url::Url::parse("https://feeds.feedburner.com/Koreus-videos").unwrap();
//! let Some(Target::Channel(feed)) = rss.parse_url(&url) else { return Ok(()) };
//! let items = rss.channels().unwrap().items(&feed, ChannelTab::Videos, None).await?;
//! let playable = rss.player().resolve(&items.items[0].r#ref).await?;
//! println!("{} streams", playable.streams.len());
//! # Ok(())
//! # }
//! ```

mod connector;
pub mod feed;
pub mod http;
pub mod id;
pub mod media;
pub mod resolver;
pub mod urls;

pub use connector::{DiscoveredFeed, RssConnector, RssConnectorBuilder};

use flaneur_model::ServiceId;

/// The connector's id, `rss`. Stored in the local database: it never changes.
#[must_use]
pub fn service_id() -> ServiceId {
    ServiceId::new("rss")
}

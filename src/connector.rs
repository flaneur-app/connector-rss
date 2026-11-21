// SPDX-License-Identifier: AGPL-3.0-or-later

//! [`RssConnector`]: the `Connector` implementation.

use std::sync::Arc;

use async_trait::async_trait;
use flaneur_connector::{
    Capabilities, ChannelTab, Channels, Connector, Credentials, Error, Player, Result, ServiceInfo,
    Session, Support, Target,
};
use flaneur_model::{
    Channel, ChannelRef, ContentFlags, Cursor, Dimensions, Item, ItemKind, ItemRef, Page, Playable,
    Playlist, ReactionCounts,
};
use url::Url;

use crate::feed::{self, ParsedFeed};
use crate::http::{DocumentKind, Http, HttpConfig};
use crate::id::ItemId;
use crate::media;
use crate::resolver::{self, FeedMedia, Resolved, Resolver, ResolverChain, SiteUrl};
use crate::{service_id, urls};

/// Builds an [`RssConnector`] with a custom HTTP configuration or extra site resolvers.
#[derive(Debug, Default)]
pub struct RssConnectorBuilder {
    http: HttpConfig,
    resolvers: Option<ResolverChain>,
}

impl RssConnectorBuilder {
    /// HTTP timeouts, cache size and revalidation delay.
    #[must_use]
    pub fn http(mut self, config: HttpConfig) -> Self {
        self.http = config;
        self
    }

    /// Replaces the resolver chain (the default is [`ResolverChain::default`]: built-in site
    /// resolvers, then the generic ones).
    #[must_use]
    pub fn resolvers(mut self, chain: ResolverChain) -> Self {
        self.resolvers = Some(chain);
        self
    }

    /// Adds a site resolver, run before the generic ones.
    #[must_use]
    pub fn resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolvers
            .get_or_insert_with(ResolverChain::default)
            .push_site(resolver);
        self
    }

    /// Builds the connector.
    pub fn build(self) -> Result<RssConnector> {
        let resolvers = self.resolvers.unwrap_or_default();
        Ok(RssConnector {
            info: ServiceInfo {
                id: service_id(),
                name: "RSS & Atom".to_owned(),
                auth_methods: Vec::new(),
            },
            http: Http::new(self.http)?,
            resolvers,
        })
    }
}

/// The RSS & Atom connector.
///
/// A feed is a channel: its [`ChannelRef`] id is the feed URL. Following feeds and the watch
/// history are kept by Flâneur itself ([`Support::Local`]); the subscription feed is the core
/// merging the [`Channels::items`] of every followed feed by date.
#[derive(Debug)]
pub struct RssConnector {
    info: ServiceInfo,
    http: Http,
    resolvers: ResolverChain,
}

impl RssConnector {
    /// The connector with the default configuration and resolvers.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    /// A builder, to configure HTTP or add site resolvers.
    #[must_use]
    pub fn builder() -> RssConnectorBuilder {
        RssConnectorBuilder::default()
    }

    /// The connector's HTTP client.
    #[must_use]
    pub fn http(&self) -> &Http {
        &self.http
    }

    /// Fetches and parses a feed, applying the site fix-ups to its items.
    async fn load_feed(&self, channel: &ChannelRef) -> Result<ParsedFeed> {
        let url = feed_url(channel)?;
        let doc = self.http.document(&url, DocumentKind::Feed).await?;
        let mut parsed = feed::parse(&url, &channel.id, doc.body.as_bytes())?;
        for entry in &mut parsed.entries {
            self.resolvers.fix_item(&mut entry.item);
        }
        Ok(parsed)
    }

    fn channel_ref(url: &Url) -> ChannelRef {
        ChannelRef::new(service_id(), url.as_str())
    }
}

fn feed_url(channel: &ChannelRef) -> Result<Url> {
    if channel.service != service_id() {
        return Err(Error::NotFound);
    }
    Url::parse(&channel.id)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or(Error::NotFound)
}

fn item_id(item: &ItemRef) -> Result<ItemId> {
    if item.service != service_id() {
        return Err(Error::NotFound);
    }
    ItemId::decode(&item.id).ok_or(Error::NotFound)
}

#[async_trait]
impl Connector for RssConnector {
    fn info(&self) -> &ServiceInfo {
        &self.info
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            signed_in: false,
            channels: Support::Anonymous,
            related: Support::Anonymous,
            subscription_feed: Support::Local,
            subscriptions_read: Support::Local,
            subscriptions_write: Support::Local,
            history_read: Support::Local,
            history_write: Support::Local,
            ..Capabilities::default()
        }
    }

    async fn sign_in(&self, credentials: Credentials) -> Result<Session> {
        let _ = credentials;
        Err(Error::Unsupported)
    }

    async fn restore(&self, session: &Session) -> Result<()> {
        let _ = session;
        Err(Error::Unsupported)
    }

    async fn sign_out(&self) {}

    fn parse_url(&self, url: &str) -> Option<Target> {
        let url = urls::normalize(&Url::parse(url.trim()).ok()?)?;
        for site in self.resolvers.sites() {
            match site.site_url(&url) {
                Some(SiteUrl::Feed(feed)) => {
                    return Some(Target::Channel(Self::channel_ref(&feed)));
                }
                Some(SiteUrl::Item { feed, page }) => {
                    let guid = page.to_string();
                    let id = ItemId::new(feed, Some(&guid), Some(page), None)?;
                    return Some(Target::Item(ItemRef::new(service_id(), id.encode())));
                }
                None => {}
            }
        }
        urls::looks_like_feed(&url).then(|| Target::Channel(Self::channel_ref(&url)))
    }

    /// The feeds behind a URL, for an "add a feed" dialog: the URL itself when it is a feed,
    /// else the feeds the page advertises (`<link rel="alternate" type="application/rss+xml">`
    /// and the Atom and JSON Feed equivalents), in page order.
    async fn discover(&self, url: &str) -> Result<Vec<Target>> {
        let url = Url::parse(url.trim())
            .ok()
            .and_then(|u| urls::normalize(&u))
            .ok_or(Error::NotFound)?;
        let doc = self.http.document(&url, DocumentKind::Page).await?;
        if feed::parse(&doc.url, url.as_str(), doc.body.as_bytes()).is_ok() {
            return Ok(vec![Target::Channel(Self::channel_ref(&url))]);
        }
        let html = scraper::Html::parse_document(&doc.body);
        let selector = media::selector(r#"link[rel~="alternate" i][href][type]"#);
        let mut found: Vec<Target> = Vec::new();
        for link in html.select(&selector) {
            let kind = media::essence(link.value().attr("type").unwrap_or(""));
            if !matches!(
                kind.as_str(),
                "application/rss+xml" | "application/atom+xml" | "application/feed+json"
            ) {
                continue;
            }
            let target = link
                .value()
                .attr("href")
                .and_then(|href| media::resolve_url(Some(&doc.url), href))
                .map(|feed| Target::Channel(Self::channel_ref(&feed)));
            if let Some(target) = target
                && !found.contains(&target)
            {
                found.push(target);
            }
        }
        Ok(found)
    }

    fn player(&self) -> &dyn Player {
        self
    }

    fn channels(&self) -> Option<&dyn Channels> {
        Some(self)
    }
}

#[async_trait]
impl Channels for RssConnector {
    async fn channel(&self, channel: &ChannelRef) -> Result<Channel> {
        Ok(self.load_feed(channel).await?.channel)
    }

    /// Feed items, newest first, in one page (feeds are not paginated). Only the `Videos` tab
    /// has items.
    async fn items(
        &self,
        channel: &ChannelRef,
        tab: ChannelTab,
        cursor: Option<&Cursor>,
    ) -> Result<Page<Item>> {
        if tab != ChannelTab::Videos || cursor.is_some() {
            return Ok(Page::last(Vec::new()));
        }
        let feed = self.load_feed(channel).await?;
        Ok(Page::last(
            feed.entries.into_iter().map(|e| e.item).collect(),
        ))
    }

    async fn playlists(
        &self,
        channel: &ChannelRef,
        cursor: Option<&Cursor>,
    ) -> Result<Page<Playlist>> {
        let _ = (channel, cursor);
        Ok(Page::last(Vec::new()))
    }
}

#[async_trait]
impl Player for RssConnector {
    /// Re-reads the feed for the item's details and enclosures, runs the resolver chain on its
    /// page, and keeps the verified streams. An item that has left the feed is resolved from its
    /// page alone.
    async fn resolve(&self, item: &ItemRef) -> Result<Playable> {
        let id = item_id(item)?;
        let mut feed = None;
        if let Some(url) = &id.feed {
            match self.load_feed(&Self::channel_ref(url)).await {
                Ok(parsed) => feed = Some(parsed),
                Err(error) if id.page.is_some() || id.media.is_some() => {
                    tracing::warn!(%url, %error, "feed failed to load; resolving the page alone");
                }
                Err(error) => return Err(error),
            }
        }
        let entry = feed.as_ref().and_then(|f| f.entry(&id.key)).cloned();
        let media = match &entry {
            Some(entry) => entry.media.clone(),
            None => id.media.clone().map(FeedMedia::url).into_iter().collect(),
        };
        let page = entry
            .as_ref()
            .and_then(|e| e.item.web_url.clone())
            .or_else(|| id.page.clone());
        if page.is_none() && media.is_empty() {
            return Err(Error::NotFound);
        }

        let resolved = self
            .resolvers
            .run(&self.http, page.as_ref(), &media)
            .await?;
        // A page that only embeds another service's player hands the item off to it; when the
        // page also has its own file (older Koreus pages), the file wins.
        let handoff = if resolved.is_empty() {
            Some(
                resolved
                    .resolution
                    .handoff
                    .clone()
                    .ok_or_else(resolver::nothing_playable)?,
            )
        } else {
            None
        };
        let mut details = match entry {
            Some(entry) => entry.item,
            None => item_from_page(item.clone(), page.clone(), feed.as_ref()),
        };
        enrich(&mut details, &resolved);
        self.resolvers.fix_item(&mut details);
        Ok(Playable {
            item: details,
            streams: resolved.streams,
            hls_manifest: resolved.hls,
            dash_manifest: resolved.dash,
            expires_at: None,
            start_at: None,
            tracking: None,
            headers: Vec::new(),
            handoff,
        })
    }

    /// The other items of the same feed: the older ones first (what comes next when reading
    /// the feed down), then the newer ones. One page.
    async fn related(&self, item: &ItemRef, cursor: Option<&Cursor>) -> Result<Page<Item>> {
        let id = item_id(item)?;
        let Some(url) = id.feed.as_ref().filter(|_| cursor.is_none()) else {
            return Ok(Page::last(Vec::new()));
        };
        let feed = self.load_feed(&Self::channel_ref(url)).await?;
        let position = feed.entries.iter().position(|e| e.id.key == id.key);
        let mut items: Vec<Item> = feed.entries.into_iter().map(|e| e.item).collect();
        if let Some(position) = position {
            items.remove(position);
            items.rotate_left(position);
        }
        Ok(Page::last(items))
    }
}

/// An item known only from its page (it left the feed, or came from a shared link).
fn item_from_page(r#ref: ItemRef, page: Option<Url>, feed: Option<&ParsedFeed>) -> Item {
    Item {
        r#ref,
        kind: ItemKind::Video,
        title: page.as_ref().map_or_default(Url::to_string),
        description: None,
        duration: None,
        published: None,
        scheduled_start: None,
        channel: feed.map(|f| f.channel.summary.clone()),
        thumbnails: Vec::new(),
        dimensions: None,
        views: None,
        reactions: ReactionCounts::default(),
        flags: ContentFlags::default(),
        web_url: page,
        watch: None,
    }
}

/// Completes an item with what the resolvers found. The feed keeps its title and date; the
/// page brings the duration, more thumbnails, the longer description and its rating.
fn enrich(item: &mut Item, resolved: &Resolved) {
    let found = &resolved.resolution;
    if item
        .web_url
        .as_ref()
        .is_some_and(|u| item.title == u.as_str())
        && let Some(title) = &found.title
    {
        item.title.clone_from(title);
    }
    item.duration = item.duration.or(found.duration);
    item.dimensions = item.dimensions.or_else(|| {
        resolved.streams.iter().find_map(|s| {
            Some(Dimensions {
                width: s.width?,
                height: s.height?,
            })
        })
    });
    item.published = item.published.or(found.published);
    if let Some(description) = &found.description
        && item
            .description
            .as_ref()
            .is_none_or(|d| d.chars().count() < description.chars().count())
    {
        item.description = Some(description.clone());
    }
    media::merge_thumbnails(&mut item.thumbnails, found.thumbnails.iter().cloned());
    item.flags.sensitive = media::combine_sensitive([item.flags.sensitive, found.sensitive]);
}

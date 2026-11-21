// SPDX-License-Identifier: AGPL-3.0-or-later

//! The connector end to end, against a local mock server serving the synthetic fixtures.

use std::time::Duration;

use flaneur_connector::{ChannelTab, Connector, Error, Support, Target};
use flaneur_connector_rss::http::{HttpConfig, USER_AGENT};
use flaneur_connector_rss::{RssConnector, service_id};
use flaneur_model::{ChannelRef, Container, ItemRef, VideoCodec};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str, base: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .replace("{{BASE}}", base)
}

/// A video file: answers a one-byte range with 206 and its full size.
async fn mount_video(server: &MockServer, at: &str, content_type: &str, size: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .and(header("range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-type", content_type)
                .insert_header("content-range", format!("bytes 0-0/{size}").as_str())
                .set_body_bytes(vec![0u8]),
        )
        .mount(server)
        .await;
}

async fn mount_text(server: &MockServer, at: &str, content_type: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", content_type)
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

async fn site() -> MockServer {
    let server = MockServer::start().await;
    let base = server.uri();
    mount_text(
        &server,
        "/rss.xml",
        "application/rss+xml",
        fixture("rss.xml", &base),
    )
    .await;
    for page in ["jsonld", "og", "oembed"] {
        let body = fixture(&format!("{page}.html"), &base);
        mount_text(
            &server,
            &format!("/videos/{page}.html"),
            "text/html; charset=utf-8",
            body,
        )
        .await;
    }
    mount_text(
        &server,
        "/oembed.json",
        "application/json",
        fixture("oembed.json", &base),
    )
    .await;
    mount_text(&server, "/embed/og", "text/html", "<html></html>".into()).await;
    mount_text(
        &server,
        "/videos/older.html",
        "text/html",
        "<html><title>Older</title></html>".into(),
    )
    .await;
    mount_video(&server, "/media/older.mp4", "video/mp4", 4_000_000).await;
    mount_video(&server, "/media/jsonld.mp4", "video/mp4", 13_000_000).await;
    mount_video(&server, "/media/og.mp4", "video/mp4", 61_000).await;
    mount_video(&server, "/media/og.webm", "video/webm", 50_000).await;
    mount_video(
        &server,
        "/media/oembed.mp4",
        "application/octet-stream",
        1_000,
    )
    .await;
    mount_video(
        &server,
        "/media/hls/master.m3u8",
        "application/vnd.apple.mpegurl",
        500,
    )
    .await;
    // `/media/broken.mp4` and `/media/missing.mp4` are not mounted: the server answers 404.
    server
}

fn connector() -> RssConnector {
    RssConnector::builder()
        .http(HttpConfig {
            timeout: Duration::from_secs(5),
            ..HttpConfig::default()
        })
        .build()
        .unwrap()
}

fn feed_ref(server: &MockServer) -> ChannelRef {
    ChannelRef::new(service_id(), format!("{}/rss.xml", server.uri()))
}

#[tokio::test]
async fn declares_local_subscriptions_and_no_accounts() {
    let rss = connector();
    assert_eq!(rss.info().id.0, "rss");
    assert_eq!(rss.info().auth_methods.len(), 0);
    let caps = rss.capabilities();
    assert_eq!(caps.channels, Support::Anonymous);
    assert_eq!(caps.related, Support::Anonymous);
    assert_eq!(caps.subscriptions_read, Support::Local);
    assert_eq!(caps.subscriptions_write, Support::Local);
    assert_eq!(caps.history_read, Support::Local);
    assert_eq!(caps.history_write, Support::Local);
    assert_eq!(caps.search, Support::Unsupported);
    assert_eq!(caps.home, Support::Unsupported);
    assert_eq!(caps.reactions_write, Support::Unsupported);
    assert_eq!(caps.comments_read, Support::Unsupported);
    assert!(rss.feeds().is_none() && rss.search().is_none() && rss.library().is_none());
    let creds = flaneur_connector::Credentials::Cookies(String::new());
    assert!(matches!(rss.sign_in(creds).await, Err(Error::Unsupported)));
}

#[tokio::test]
async fn lists_feed_items_newest_first() {
    let server = site().await;
    let rss = connector();
    let channels = rss.channels().unwrap();
    let feed = feed_ref(&server);

    let channel = channels.channel(&feed).await.unwrap();
    assert_eq!(channel.summary.name, "Example & Videos");
    assert_eq!(channel.summary.r#ref, feed);
    assert_eq!(channel.description.as_deref(), Some("Short films, weekly."));
    assert_eq!(channel.summary.avatars[0].url.path(), "/logo.png");

    let page = channels
        .items(&feed, ChannelTab::Videos, None)
        .await
        .unwrap();
    assert!(page.next.is_none());
    let titles: Vec<_> = page.items.iter().map(|i| i.title.as_str()).collect();
    // The audio episode is left out.
    assert_eq!(
        titles,
        [
            "Newest clip, page only",
            "Middle clip, Media RSS",
            "Older clip with an enclosure"
        ]
    );
    let middle = &page.items[1];
    assert_eq!(middle.duration, Some(61));
    assert_eq!(middle.flags.sensitive, Some(true)); // media:rating adult
    assert_eq!(middle.channel.as_ref().unwrap().name, "Example & Videos");
    let older = &page.items[2];
    assert_eq!(older.thumbnails[0].url.path(), "/thumbs/older.jpg");
    assert_eq!(older.thumbnails[0].width, Some(320));

    let shorts = channels
        .items(&feed, ChannelTab::Shorts, None)
        .await
        .unwrap();
    assert_eq!(shorts.items.len(), 0);
}

#[tokio::test]
async fn resolves_through_json_ld() {
    let server = site().await;
    let rss = connector();
    let items = rss
        .channels()
        .unwrap()
        .items(&feed_ref(&server), ChannelTab::Videos, None)
        .await
        .unwrap();
    let newest = &items.items[0];

    let playable = rss.player().resolve(&newest.r#ref).await.unwrap();
    assert_eq!(playable.streams.len(), 1);
    let stream = &playable.streams[0];
    assert_eq!(
        stream.url.as_str(),
        format!("{}/media/jsonld.mp4", server.uri())
    );
    assert_eq!(stream.container, Container::Mp4);
    assert_eq!((stream.width, stream.height), (Some(1920), Some(1080)));
    assert_eq!(stream.size, Some(13_000_000));
    assert_eq!(stream.bitrate, Some(1_600_000)); // size * 8 / 65 s
    let item = &playable.item;
    assert_eq!(item.title, "Newest clip, page only"); // the feed's title wins
    assert_eq!(item.duration, Some(65));
    assert_eq!(item.flags.sensitive, Some(true)); // isFamilyFriendly: false
    assert!(
        item.description
            .as_deref()
            .unwrap()
            .contains("longer description")
    );
    assert!(
        item.thumbnails
            .iter()
            .any(|t| t.url.path() == "/thumbs/jsonld.jpg" && t.width == Some(1280))
    );
}

#[tokio::test]
async fn verifies_every_candidate() {
    let server = site().await;
    let rss = connector();
    let items = rss
        .channels()
        .unwrap()
        .items(&feed_ref(&server), ChannelTab::Videos, None)
        .await
        .unwrap();
    let middle = &items.items[1];

    let playable = rss.player().resolve(&middle.r#ref).await.unwrap();
    let paths: Vec<_> = playable.streams.iter().map(|s| s.url.path()).collect();
    // The feed's `broken.mp4` (404), the `text/html` embed and `missing.mp4` are dropped;
    // `og.mp4` appears once although both `og:video` and `<source>` give it.
    assert_eq!(paths, ["/media/og.mp4", "/media/og.webm"]);
    assert_eq!(
        playable.hls_manifest.as_ref().unwrap().path(),
        "/media/hls/master.m3u8"
    );
    let webm = &playable.streams[1];
    assert_eq!(webm.container, Container::WebM);
    assert_eq!(webm.video_codec, Some(VideoCodec::Vp9));
    assert_eq!(webm.audio.as_ref().unwrap().codec.as_deref(), Some("opus"));
    assert_eq!(
        (playable.streams[0].width, playable.streams[0].height),
        (Some(1280), Some(720))
    );
    // The feed says adult, the page says general: any `true` wins.
    assert_eq!(playable.item.flags.sensitive, Some(true));

    let requests = server.received_requests().await.unwrap();
    let probes = requests
        .iter()
        .filter(|r| r.headers.get("range").is_some())
        .count();
    assert!(probes >= 5, "every candidate is probed ({probes})");
    assert!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/embed/og")
            .all(|r| r.headers.get("range").is_none()),
        "a candidate declared text/html is not probed"
    );
    assert!(requests.iter().all(user_agent_is_polite));
}

fn user_agent_is_polite(request: &Request) -> bool {
    request
        .headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        == Some(USER_AGENT)
}

#[tokio::test]
async fn falls_back_to_oembed() {
    let server = site().await;
    let base = server.uri();
    let rss = connector();
    let page = Url::parse(&format!("{base}/videos/oembed.html")).unwrap();
    let id = flaneur_connector_rss::id::ItemId::new(None, None, Some(page), None).unwrap();
    let item = ItemRef::new(service_id(), id.encode());

    let playable = rss.player().resolve(&item).await.unwrap();
    assert_eq!(playable.streams[0].url.path(), "/media/oembed.mp4");
    assert_eq!(playable.item.title, "oEmbed clip");
    assert_eq!(playable.item.thumbnails[0].width, Some(640));
    assert!(playable.item.channel.is_none());
}

#[tokio::test]
async fn related_lists_the_rest_of_the_feed() {
    let server = site().await;
    let rss = connector();
    let items = rss
        .channels()
        .unwrap()
        .items(&feed_ref(&server), ChannelTab::Videos, None)
        .await
        .unwrap();
    let related = rss
        .player()
        .related(&items.items[1].r#ref, None)
        .await
        .unwrap();
    let titles: Vec<_> = related.items.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Older clip with an enclosure", "Newest clip, page only"]
    );
}

#[tokio::test]
async fn item_without_anything_playable_is_unavailable() {
    let server = site().await;
    let rss = connector();
    let page = Url::parse(&format!("{}/videos/older.html", server.uri())).unwrap();
    let id = flaneur_connector_rss::id::ItemId::new(None, None, Some(page), None).unwrap();
    let result = rss
        .player()
        .resolve(&ItemRef::new(service_id(), id.encode()))
        .await;
    assert!(matches!(result, Err(Error::Unavailable(_))), "{result:?}");
    let foreign = ItemRef::new(service_id(), "dQw4w9WgXcQ");
    assert!(matches!(
        rss.player().resolve(&foreign).await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn revalidates_with_etag() {
    let server = MockServer::start().await;
    let body = fixture("rss.xml", &server.uri());
    Mock::given(method("GET"))
        .and(path("/rss.xml"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"v1\""))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rss.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/rss+xml")
                .insert_header("etag", "\"v1\"")
                .set_body_string(body),
        )
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    let rss = RssConnector::builder()
        .http(HttpConfig {
            revalidate_after: Duration::ZERO,
            ..HttpConfig::default()
        })
        .build()
        .unwrap();
    let feed = feed_ref(&server);
    let first = rss.channels().unwrap().channel(&feed).await.unwrap();
    let second = rss.channels().unwrap().channel(&feed).await.unwrap();
    assert_eq!(first, second);
    // `expect(1)` on both mocks is checked when the server drops.
}

#[tokio::test]
async fn recognizes_urls() {
    let rss = connector();
    let parse = |s: &str| rss.parse_url(&Url::parse(s).unwrap());

    let Some(Target::Item(item)) = parse("http://koreus.com/media/illusion-tandem.html") else {
        panic!("a Koreus page is an item");
    };
    let id = flaneur_connector_rss::id::ItemId::decode(&item.id).unwrap();
    assert_eq!(
        id.feed.unwrap().as_str(),
        "https://feeds.feedburner.com/Koreus-videos"
    );
    assert_eq!(id.key, "https://www.koreus.com/video/illusion-tandem.html");

    assert_eq!(
        parse("feed://example.org/videos.rss"),
        Some(Target::Channel(ChannelRef::new(
            service_id(),
            "https://example.org/videos.rss"
        )))
    );
    assert_eq!(
        parse("http://feeds.feedburner.com/Koreus-videos"),
        Some(Target::Channel(ChannelRef::new(
            service_id(),
            "https://feeds.feedburner.com/Koreus-videos"
        )))
    );
    assert_eq!(parse("https://example.org/some/article.html"), None);
}

#[tokio::test]
async fn discovers_feeds_of_a_page() {
    let server = MockServer::start().await;
    let html = r#"<html><head>
        <link rel="alternate" type="application/rss+xml" href="/videos.rss" title="Videos">
        <link rel="alternate" type="application/atom+xml" href="/atom.xml">
        <link rel="alternate" type="text/html" href="/en/">
        </head></html>"#;
    mount_text(&server, "/", "text/html", html.into()).await;
    let rss = connector();
    let found = rss
        .discover_feeds(&Url::parse(&server.uri()).unwrap())
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].url.path(), "/videos.rss");
    assert_eq!(found[0].title.as_deref(), Some("Videos"));
}

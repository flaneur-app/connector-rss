// SPDX-License-Identifier: AGPL-3.0-or-later

//! Offline tests against saved fixtures: the real Koreus feed and pages (trimmed), and a
//! synthetic Atom feed. Nothing here touches the network.

use flaneur_connector_rss::feed;
use flaneur_connector_rss::http::{Http, HttpConfig};
use flaneur_connector_rss::resolver::embed::EmbeddedPlayer;
use flaneur_connector_rss::resolver::koreus::{self, Koreus};
use flaneur_connector_rss::resolver::meta::MetaTags;
use flaneur_connector_rss::resolver::microdata::Microdata;
use flaneur_connector_rss::resolver::{PageDocument, ResolveRequest, Resolver, ResolverChain};
use flaneur_model::{Precision, VideoCodec};
use url::Url;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn page(name: &str, url: &str) -> PageDocument {
    PageDocument {
        url: Url::parse(url).unwrap(),
        source: fixture(name).into(),
    }
}

async fn run(
    resolver: &dyn Resolver,
    page: &PageDocument,
) -> flaneur_connector_rss::resolver::Resolution {
    let http = Http::new(HttpConfig::default()).unwrap();
    resolver
        .resolve(&ResolveRequest {
            page_url: Some(&page.url),
            feed_media: &[],
            page: Some(page),
            http: &http,
        })
        .await
        .unwrap()
}

#[test]
fn koreus_feed_maps_to_items() {
    let url = Url::parse(koreus::FEED).unwrap();
    let mut parsed =
        feed::parse(&url, koreus::FEED, fixture("koreus-feed.xml").as_bytes()).unwrap();
    let chain = ResolverChain::default();
    for entry in &mut parsed.entries {
        chain.fix_item(&mut entry.item);
    }

    assert_eq!(
        parsed.channel.summary.name,
        "Koreus.com - Les dernières vidéos"
    );
    assert_eq!(
        parsed.channel.description.as_deref(),
        Some("Vidéos insolites")
    );
    assert_eq!(
        parsed.channel.web_url.as_ref().unwrap().as_str(),
        "https://www.koreus.com/"
    );
    assert_eq!(parsed.channel.summary.avatars[0].width, Some(144));
    assert_eq!(parsed.entries.len(), 3);

    let first = &parsed.entries[0];
    let item = &first.item;
    assert_eq!(item.title, "Illusion sur un tandem");
    assert_eq!(
        item.web_url.as_ref().unwrap().as_str(),
        "https://www.koreus.com/video/illusion-tandem.html"
    );
    let published = item.published.unwrap();
    assert_eq!(published.precision, Precision::Exact);
    assert_eq!(published.at, 1_795_171_721); // Fri, 20 Nov 2026 11:48:41 +0100
    assert_eq!(item.duration, None);
    assert_eq!(item.flags.sensitive, None);
    assert!(
        item.description
            .as_deref()
            .unwrap()
            .starts_with("Lors d'une course cycliste")
    );
    assert!(!item.description.as_deref().unwrap().contains('<'));

    // The description's `<img>` (sized) and the repaired `media:thumbnail`.
    let thumbs: Vec<_> = item.thumbnails.iter().map(|t| t.url.as_str()).collect();
    assert!(thumbs.contains(&"https://koreus.b-cdn.net/thumbshigh/202611/illusion-tandem.jpg"));
    assert!(thumbs.contains(&"https://koreus.b-cdn.net/thumbs/202611/illusion-tandem.jpg"));
    assert!(thumbs.iter().all(|t| !t.ends_with("tandemjpg")));
    assert!(item.thumbnails.iter().any(|t| t.width == Some(640)));

    // `media:content`, with the vertical size the feed gives.
    assert_eq!(first.media.len(), 1);
    assert_eq!(
        first.media[0].url.as_str(),
        "https://embed.koreus.com/00071/202611/illusion-tandem.mp4"
    );
    assert_eq!(
        (first.media[0].width, first.media[0].height),
        (Some(720), Some(1280))
    );
    assert!(first.media[0].is_video());
    // Koreus's many vertical videos are recognizable from the feed alone.
    assert!(item.dimensions.unwrap().is_portrait());

    // The guid is the link, so the id holds the feed and the link only.
    assert_eq!(
        first.id.key,
        "https://www.koreus.com/video/illusion-tandem.html"
    );
    assert_eq!(first.id.media, None);
    assert!(
        item.r#ref
            .id
            .starts_with("feed=https%3A%2F%2Ffeeds.feedburner.com%2FKoreus-videos&key=")
    );

    // Newest first.
    let dates: Vec<_> = parsed
        .entries
        .iter()
        .map(|e| e.item.published.unwrap().at)
        .collect();
    assert!(dates.windows(2).all(|w| w[0] >= w[1]));
}

#[tokio::test]
async fn koreus_resolver_reads_player_variants() {
    let page = page(
        "koreus-page.html",
        "https://www.koreus.com/video/illusion-tandem.html",
    );
    assert!(Koreus.applies_to(Some(&page.url)));
    let found = run(&Koreus, &page).await;
    let urls: Vec<_> = found.candidates.iter().map(|c| c.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://embed.koreus.com/00071/202611/illusion-tandem.vp9.mp4",
            "https://embed.koreus.com/00071/202611/illusion-tandem.mp4",
        ]
    );
    let vp9 = &found.candidates[0];
    assert_eq!(vp9.label.as_deref(), Some("vp09"));
    assert_eq!(
        vp9.mime.as_deref(),
        Some("video/mp4; codecs=vp09, mp4a.40.2")
    );
    // No HD variant: every variant has the page's size.
    assert!(
        found
            .candidates
            .iter()
            .all(|c| (c.width, c.height) == (Some(720), Some(1280)))
    );
    let codecs = flaneur_connector_rss::media::codecs(found.candidates[1].mime.as_deref().unwrap());
    assert_eq!(
        flaneur_connector_rss::media::video_codec(&codecs),
        Some(VideoCodec::H264)
    );
}

#[tokio::test]
async fn koreus_resolver_ranks_hd_first() {
    let page = page(
        "koreus-page-hd.html",
        "https://www.koreus.com/video/spectateur-chute-cycliste-sprint.html",
    );
    let found = run(&Koreus, &page).await;
    assert_eq!(found.candidates.len(), 2);
    let hd = &found.candidates[0];
    assert!(hd.url.as_str().ends_with("-hd.mp4"));
    assert_eq!(hd.label.as_deref(), Some("avc1 HD"));
    assert_eq!((hd.width, hd.height), (Some(1280), Some(720)));
    // The SD file is smaller, by an amount the page does not say.
    let sd = &found.candidates[1];
    assert_eq!((sd.width, sd.height), (None, None));
    assert_eq!(sd.label.as_deref(), Some("avc1"));
    // The page also embeds the same video from YouTube; it is kept as a handoff, which the
    // chain uses only when none of the page's own files plays.
    let embedded = run(&EmbeddedPlayer, &page).await;
    assert_eq!(embedded.handoff.unwrap().to_string(), "youtube:hH7HeBpPbQA");
}

#[tokio::test]
async fn koreus_page_with_a_dailymotion_copy() {
    // The real page: its own MP4 variants in `playerkoreus()`, and a Dailymotion player built by
    // `playerori()`. The variants are the candidates; the embed is only kept as a handoff.
    let page = page(
        "koreus-page-dailymotion.html",
        "https://www.koreus.com/video/chat-mordre-main.html",
    );
    let own = run(&Koreus, &page).await;
    assert_eq!(own.candidates.len(), 2);
    assert!(
        own.candidates
            .iter()
            .all(|c| c.url.host_str() == Some("embed.koreus.com"))
    );
    let embedded = run(&EmbeddedPlayer, &page).await;
    assert_eq!(embedded.handoff.unwrap().to_string(), "dailymotion:xby943q");
    // One embedded video, found once.
    assert_eq!(
        flaneur_connector_rss::resolver::embed::all(&page.source).len(),
        1
    );
}

#[tokio::test]
async fn generic_resolvers_read_koreus_page() {
    let page = page(
        "koreus-page.html",
        "https://www.koreus.com/video/illusion-tandem.html",
    );

    let micro = run(&Microdata, &page).await;
    assert_eq!(micro.duration, Some(20));
    assert_eq!(micro.title.as_deref(), Some("Illusion sur un tandem"));
    let uploaded = micro.published.unwrap();
    assert_eq!(
        (uploaded.at, uploaded.precision),
        (1_795_171_721, Precision::Exact)
    );
    assert_eq!(
        micro.candidates[0].url.as_str(),
        "https://embed.koreus.com/00071/202611/illusion-tandem.mp4"
    );
    assert!(
        micro
            .description
            .as_deref()
            .unwrap()
            .contains("complémentarité")
    );

    let meta = run(&MetaTags, &page).await;
    assert_eq!(meta.sensitive, Some(false)); // <meta name="rating" content="general">
    assert_eq!(meta.title.as_deref(), Some("Illusion sur un tandem"));
    assert_eq!(meta.candidates.len(), 1);
    assert_eq!(meta.candidates[0].mime.as_deref(), Some("video/mp4"));
    assert_eq!(
        meta.thumbnails[0].url.as_str(),
        "https://koreus.b-cdn.net/thumbshigh/202611/illusion-tandem.jpg"
    );
}

#[test]
fn atom_feed_maps_enclosures_and_skips_photos() {
    let url = Url::parse("https://example.org/atom.xml").unwrap();
    let parsed = feed::parse(&url, url.as_str(), fixture("atom.xml").as_bytes()).unwrap();
    assert_eq!(parsed.channel.summary.name, "Atom Example");
    assert_eq!(
        parsed.channel.web_url.as_ref().unwrap().as_str(),
        "https://example.org/"
    );
    assert_eq!(parsed.entries.len(), 2, "the photo entry is left out");

    let one = &parsed.entries[0];
    assert_eq!(one.item.title, "Clip one");
    assert_eq!(one.id.key, "tag:example.org,2026:1");
    assert_eq!(
        one.id.page.as_ref().unwrap().as_str(),
        "https://example.org/clips/1"
    );
    assert_eq!(one.media[0].url.as_str(), "https://cdn.example.org/1.webm");
    assert_eq!(one.media[0].size, Some(2048));
    assert_eq!(one.item.description.as_deref(), Some("First clip."));
    // `published` wins over `updated`.
    assert_eq!(one.item.published.unwrap().at, 1_795_167_000);

    let two = &parsed.entries[1];
    assert_eq!(
        two.item.description.as_deref(),
        Some("Second clip, from Media RSS.")
    );
    assert_eq!(two.item.thumbnails[0].width, Some(1280));
}

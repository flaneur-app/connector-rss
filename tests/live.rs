// SPDX-License-Identifier: AGPL-3.0-or-later

//! Live test against Koreus. Ignored by default: run it with
//! `cargo nextest run --run-ignored only` (it needs the network, and the site may change).

use flaneur_connector::{ChannelTab, Connector, Target};
use flaneur_connector_rss::RssConnector;
use flaneur_model::VideoCodec;
use url::Url;

#[tokio::test]
#[ignore = "needs the network; hits feeds.feedburner.com and www.koreus.com"]
async fn koreus_feed_resolves_to_verified_streams() {
    let rss = RssConnector::new().unwrap();
    let url = Url::parse("https://feeds.feedburner.com/Koreus-videos").unwrap();
    let Some(Target::Channel(feed)) = rss.parse_url(&url) else {
        panic!("the Koreus feed is a channel");
    };
    let channels = rss.channels().unwrap();
    let header = channels.channel(&feed).await.unwrap();
    assert!(header.summary.name.contains("Koreus"));

    let items = channels
        .items(&feed, ChannelTab::Videos, None)
        .await
        .unwrap();
    assert_ne!(items.items.len(), 0);
    for item in &items.items {
        assert!(item.published.is_some());
        assert!(
            item.thumbnails.iter().all(|t| t
                .url
                .path()
                .rsplit('/')
                .next()
                .is_some_and(|f| f.contains('.'))),
            "broken thumbnails are repaired"
        );
    }

    let first = &items.items[0];
    let playable = rss.player().resolve(&first.r#ref).await.unwrap();
    assert_ne!(playable.streams.len(), 0);
    assert!(
        playable.item.duration.is_some(),
        "the page gives the duration"
    );
    assert_eq!(
        playable.item.flags.sensitive,
        Some(false),
        "rating: general"
    );
    assert!(playable.streams.iter().all(|s| s.size.is_some()));
    assert!(
        playable
            .streams
            .iter()
            .any(|s| s.video_codec == Some(VideoCodec::H264)),
        "an H.264 variant is listed"
    );

    // A shared link to the same page resolves to the same item.
    let page = first.web_url.as_ref().unwrap();
    assert_eq!(rss.parse_url(page), Some(Target::Item(first.r#ref.clone())));

    let related = rss.player().related(&first.r#ref, None).await.unwrap();
    assert_eq!(related.items.len(), items.items.len() - 1);
}

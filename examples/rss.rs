// SPDX-License-Identifier: AGPL-3.0-or-later

//! Prints the items of a feed and resolves the first one.
//!
//! ```sh
//! cargo run --example rss [FEED_OR_PAGE_URL]
//! ```
//!
//! Without an argument, it reads the Koreus video feed. A page URL that is not a feed is
//! searched for the feeds it advertises.

use flaneur_connector::{ChannelTab, Connector, Target};
use flaneur_connector_rss::RssConnector;
use flaneur_model::Item;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://feeds.feedburner.com/Koreus-videos".to_owned());
    let rss = RssConnector::new()?;

    let feed = if let Some(Target::Channel(feed)) = rss.parse_url(&arg) {
        feed
    } else {
        let found = rss.discover(&arg).await?;
        let Some(Target::Channel(first)) = found.into_iter().next() else {
            return Err("no feed found at this URL".into());
        };
        println!("Using the feed advertised by the page: {}", first.id);
        first
    };

    let channels = rss.channels().expect("the RSS connector has channels");
    let channel = channels.channel(&feed).await?;
    println!("# {}", channel.summary.name);
    if let Some(description) = &channel.description {
        println!("{description}");
    }
    println!();

    let items = channels.items(&feed, ChannelTab::Videos, None).await?;
    for item in &items.items {
        print_item(item);
    }

    let Some(first) = items.items.first() else {
        println!("No video items.");
        return Ok(());
    };
    println!("\n## Resolving: {}", first.title);
    let playable = rss.player().resolve(&first.r#ref).await?;
    let item = &playable.item;
    println!("duration:  {:?} s", item.duration);
    println!("sensitive: {:?}", item.flags.sensitive);
    println!(
        "frame:     {}",
        item.dimensions.map_or_else(
            || "unknown".to_owned(),
            |d| format!(
                "{}x{}{}",
                d.width,
                d.height,
                if d.is_portrait() { " (portrait)" } else { "" }
            )
        )
    );
    if let Some(other) = &playable.handoff {
        println!("handoff:   {other}");
    }
    for thumb in &item.thumbnails {
        println!(
            "thumbnail: {} {}",
            thumb.url,
            size(thumb.width, thumb.height)
        );
    }
    for stream in &playable.streams {
        println!(
            "stream:    {} [{}] {:?} {:?} {} size={:?} bitrate={:?} audio={:?}",
            stream.url,
            stream.label.as_deref().unwrap_or("-"),
            stream.container,
            stream.video_codec,
            size(stream.width, stream.height),
            stream.size,
            stream.bitrate,
            stream.audio.as_ref().and_then(|a| a.codec.as_deref()),
        );
    }
    if let Some(hls) = &playable.hls_manifest {
        println!("hls:       {hls}");
    }
    if let Some(dash) = &playable.dash_manifest {
        println!("dash:      {dash}");
    }

    let related = rss.player().related(&first.r#ref, None).await?;
    println!("related:   {} items", related.items.len());
    Ok(())
}

fn size(width: Option<u32>, height: Option<u32>) -> String {
    let side = |v: Option<u32>| v.map_or_else(|| "?".to_owned(), |v| v.to_string());
    format!("{}x{}", side(width), side(height))
}

fn print_item(item: &Item) {
    let date = item
        .published
        .and_then(|p| jiff::Timestamp::from_second(p.at).ok())
        .map_or_else(
            || "????-??-??".to_owned(),
            |d| d.strftime("%Y-%m-%d %H:%M UTC").to_string(),
        );
    let page = item.web_url.as_ref().map_or("", Url::as_str);
    println!("- {date}  {}  <{page}>", item.title);
}

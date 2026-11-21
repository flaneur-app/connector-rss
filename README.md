# Flâneur connector: RSS & Atom

The generic feed connector of [Flâneur](https://github.com/flaneur-app): RSS and Atom subscriptions, plus page resolvers that find the video file behind a feed item on sites that host their own videos.

> Flâneur is an independent, open-source project. It is not affiliated with, endorsed by or
> sponsored by Google, YouTube, Odysee, LBRY or any other service it can connect to. All
> trademarks belong to their owners.

## What it supports

The crate `flaneur-connector-rss` implements the `Connector` trait of
[`flaneur-app/core`](https://github.com/flaneur-app/core) with the service id `rss`.

| Operation | Support | How |
| --- | --- | --- |
| Channel pages | Anonymous | A feed is a channel: its id is the feed URL. The header comes from the feed (title, description, logo, site link); the `Videos` tab lists the feed's video items, newest first, in one page. |
| Playback | Anonymous | `resolve` re-reads the feed, runs the resolver chain on the item's enclosures and page, and returns the verified streams (`StreamKind::Muxed`), HLS and DASH manifests. |
| Related items | Anonymous | The other items of the same feed: older ones first, then newer ones. |
| Following feeds | Local | Kept by the core's store; the subscription feed is the core merging the items of every followed feed by date. |
| History | Local | Kept by the core's store. |
| Search, home, trending, playlists, reactions, comments | Unsupported | |

There are no accounts: `auth_methods` is empty and `sign_in` returns `Error::Unsupported`.

Feeds: RSS 0.9x/1.0/2.0, Atom and JSON Feed (parsed with [feed-rs](https://crates.io/crates/feed-rs)),
with RSS `<enclosure>`, Media RSS (`media:content`, `media:group`, `media:thumbnail`,
`media:rating`) and Atom `rel="enclosure"` links. Items whose only media are audio or images are
left out; items without media but with a page are kept, for the resolvers.

Items: kind `Video`, title, plain-text description, publication date (`Precision::Exact` from
feeds, `Day` for date-only page dates), duration when the feed or page gives it, thumbnails
(Media RSS, the description's first `<img>`, page images), and `flags.sensitive` from
`media:rating`, `<meta name="rating">`, JSON-LD `isFamilyFriendly` / `contentRating` and
`og:restrictions:age` (`adult`, `mature`... → `Some(true)`, `general` → `Some(false)`, nothing →
`None`; when sources disagree, `Some(true)` wins).

`parse_url` recognizes feed URLs by their shape (`.rss`, `.xml`, `/feed`, `feeds.*` hosts,
`feed://`...) and the pages of sites with a site resolver. It cannot fetch anything, so pages of
unknown sites are not claimed: `RssConnector::discover_feeds` lists the feeds a page advertises,
for an "add a feed" dialog. Apps should ask this connector last, after the service connectors.

### Item ids

`ItemRef.id` encodes the feed URL and the item's identity (its `guid`, else its link), plus its
page or media URL when they differ, as a form-urlencoded string. It is stable as long as the feed
URL and the item's guid do not change, and it is enough to resolve an item that has left the
feed. See [`src/id.rs`](src/id.rs) for the exact format.

### HTTP

One shared client (rustls with `ring` and the platform's certificate verifier, gzip, HTTP/2),
20 s timeout, and the User-Agent `Flaneur/<version> (+https://flaneur.baxyz.dev)`, which names
the client and nothing about the user. Feeds and pages are cached in memory and revalidated with
`If-None-Match` / `If-Modified-Since`; a copy younger than 60 s is reused without a request, so
a channel page followed by `resolve` and `related` reads the feed once. Media URLs are probed two
at a time, at most eight per item.

## How resolvers work

A `Resolver` gets the item's page URL, what the feed attached to the item, the fetched page and
the HTTP client, and returns candidate media URLs plus metadata (title, description, duration,
upload date, thumbnails, sensitivity). The chain runs:

1. **site resolvers** whose `applies_to` accepts the page (built in: `koreus`);
2. the **generic resolvers**, in this order:
   1. `enclosure`: the feed's `<enclosure>` / `media:content` / Atom enclosure;
   2. `json-ld`: JSON-LD `VideoObject`, also inside `@graph` or nested objects;
   3. `microdata`: schema.org `VideoObject` microdata;
   4. `meta-tags`: `og:video` (an `og:video:type` that is not video, such as `text/html`, rules
      the URL out), `twitter:player:stream`, `og:image`, `video:duration`, `<meta name="rating">`;
   5. `html5-video`: `<video src>`, `<video>` / `<video-js>` `<source>` elements;
   6. `oembed`: the page's JSON oEmbed document, only when nothing before found a candidate.

The page is fetched once, and not at all when the link points at a media file. Results merge
field by field, earlier resolvers first; candidates with the same URL merge into one. A site
resolver can be **authoritative**: once it finds candidates, later resolvers add metadata but
no candidates.

Nothing a page claims is trusted: every candidate is **verified** with a `GET` and
`Range: bytes=0-0` (not `HEAD`, which some CDNs refuse), and kept only if it answers 200/206 with
a video, HLS or DASH content type (`application/octet-stream` only for URLs with a video
extension). The probe also gives the file size (`Content-Range`), and the bitrate follows from
size and duration.

### Koreus

The `koreus` resolver covers [Koreus](https://www.koreus.com), whose feed
(`https://feeds.feedburner.com/Koreus-videos`) attaches the MP4 to each item. The generic
resolvers already find that file, the duration (microdata) and the rating (`general` →
`sensitive: Some(false)`); the site resolver adds:

- the player variants (VP9 `vp09`, H.264 `avc1`, `avc1 HD` on older pages), which sit in a
  JavaScript string rather than in the DOM, as streams with their codecs; the page's size goes to
  the HD variant when there is one, to every variant otherwise;
- repaired thumbnails: the feed's `media:thumbnail` URLs lack the dot before `jpg`;
- `parse_url` for `/video/<slug>.html` (and the old `/media/<slug>.html`) pages, mapped to the
  same item ids the feed produces.

Known point for the maintainer to decide on: Koreus is funded by advertising on its pages, and
playing the MP4 file directly, as this connector does, skips the page and its ads. The item's
`web_url` links back to the page.

### Adding a site resolver

1. Add a module under `src/resolver/` with a unit struct implementing `Resolver`:
   - `name()`: a short name for logs;
   - `applies_to(page)`: match the site's host;
   - `resolve(request)`: read `request.page` (call `page.html()` inside a synchronous block:
     `scraper::Html` is not `Send`) and return a `Resolution`; reuse `html5::read`,
     `meta::read` or the `media` helpers where they fit;
   - optionally `authoritative()`, `fix_item(item)` (repairs to the feed's items) and
     `site_url(url)` (the site's feed and video pages, for `parse_url`).
2. Register it in `ResolverChain::default()` (built in), or at run time with
   `RssConnector::builder().resolver(Arc::new(MySite)).build()`.
3. Save a trimmed copy of a real page under `tests/fixtures/` and test the resolver against it;
   add an `#[ignore]` live test if the site is worth watching.

Delegating to another connector (a page that only embeds a YouTube or PeerTube player) is not
supported yet.

## Example

```sh
cargo run --example rss                       # the Koreus video feed
cargo run --example rss -- https://framatube.org/feeds/videos.xml
cargo run --example rss -- https://example.org/   # a page: uses the first feed it advertises
```

It prints the feed's items, resolves the first one (streams, codecs, sizes, duration,
sensitivity) and counts its related items.

## Development

See [AGENTS.md](AGENTS.md) for the conventions and the commands. The shared dev container lives in
[`flaneur-app/.dev`](https://github.com/flaneur-app/.dev).

## License

[AGPL-3.0-or-later](LICENSE).

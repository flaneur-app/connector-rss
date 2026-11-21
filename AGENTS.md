# AGENTS.md — Flâneur `connector-rss`

This repository inherits the [canonical workspace rules](https://github.com/flaneur-app/.dev/blob/main/AGENTS.md). Only project-specific details are documented here.

## Scope

The generic feed connector of [Flâneur](https://github.com/flaneur-app): RSS and Atom subscriptions, plus page resolvers that find the video file behind a feed item on sites that host their own videos.

**Status:** first version: channels (feeds), playback through the resolver chain, related items.
Subscriptions and history are local (kept by the core).

## Structure

```text
src/
├── lib.rs              # crate docs, module list, service_id()
├── connector.rs        # RssConnector: Connector, Channels, Player; builder; feed discovery
├── feed.rs             # feed-rs parsing and mapping into Channel / Item
├── id.rs               # ItemRef.id encoding (feed, key, page, media)
├── http.rs             # shared client, User-Agent, cache with ETag/Last-Modified, ranged probes
├── media.rs            # MIME/codecs, ISO 8601 durations and dates, ratings, HTML to text
├── urls.rs             # feed URL shapes for parse_url
└── resolver/
    ├── mod.rs          # Resolver trait, ResolverChain, merging and verification
    ├── enclosure.rs    # feed enclosures and Media RSS
    ├── jsonld.rs       # JSON-LD VideoObject
    ├── microdata.rs    # schema.org microdata
    ├── schema.rs       # VideoObject mapping shared by the two above
    ├── meta.rs         # Open Graph, Twitter card, <meta name="rating">
    ├── html5.rs        # <video> / <source>
    ├── embed.rs        # YouTube embeds, as Playable.handoff
    ├── oembed.rs       # oEmbed (fallback)
    └── koreus.rs       # site resolver: www.koreus.com
tests/
├── fixtures/           # trimmed real Koreus feed and pages; synthetic RSS/Atom/JSON-LD/OG/oEmbed
├── fixtures.rs         # offline tests on the fixtures
├── connector.rs        # end to end against a local mock server (wiremock)
└── live.rs             # #[ignore] live test against Koreus
examples/rss.rs         # prints a feed's items and resolves the first one
```

The core crates (`flaneur-model`, `flaneur-connector`) come from git,
`https://github.com/flaneur-app/core`, branch `feat/core-structure`. Never edit them from here:
report trait issues on the core repository.

## License

**AGPL-3.0-or-later**. Every source file starts with an SPDX header:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
```

Synthetic fixtures carry it in an XML/HTML comment. The Koreus fixtures are trimmed copies of
third-party pages kept for tests only, and carry their source URL instead.

Dependencies must have AGPL-compatible licenses; `cargo deny check` enforces the list in `deny.toml`.
TLS uses rustls with the `ring` provider rather than reqwest's default `aws-lc-rs`, whose
OpenSSL-licensed parts are not on the allowed list.

## Naming and legal positioning

- Never use "YouTube" or "tube" in the name of the app, a crate meant for end users, or the org.
  Service names appear only where they describe what a connector talks to.
- No ad blocking and no downloading as advertised features.
- Cookies and tokens stay on the user's machine (OS keyring). No code path sends them anywhere but
  to the service they belong to.
- Every README keeps the non-affiliation notice.
- The User-Agent names the client (`Flaneur/<version> (+https://flaneur.baxyz.dev)`) and never
  anything about the user.

## Commit Scopes

Defined in `scopes.json`:

| Scope | Covers |
| --- | --- |
| `connector` | Connector implementation and capabilities |
| `feed` | RSS/Atom fetching and parsing |
| `resolver` | Page resolvers: generic strategies and per-site resolvers |
| `build`, `test`, `docs`, `deps`, `ci`, `ai` | As usual |

## Commands

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --no-tests=pass
cargo deny check
cargo nextest run --run-ignored only       # live test (network)
cargo run --example rss [FEED_OR_PAGE_URL]  # live example
```

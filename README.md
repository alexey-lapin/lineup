# lineup

lineup manages IPTV channels and playlists in place of a hand-edited M3U file. It serves each
playlist as M3U and its guide as XMLTV over HTTP, for anything that reads them from a URL: Jellyfin
and Emby Live TV, Kodi with IPTV Simple, IPTV apps such as TiviMate.

Each channel plays through a **route** you pick: **direct** (a stream URL, put in playlists as it
is) or a **gateway**, a separate service that turns a reference (an id, a page link) into a
playable URL. lineup itself knows no streaming protocol; gateways speak a small contract (crate
`gateway-contract`). This repository includes one, [lineup-gateway](docs/gateway.md).

One Rust binary serves:

- a web UI, installable on an iPhone home screen;
- playlists as M3U, each with its own XMLTV guide, built from guide sources lineup downloads itself;
- a REST API and server-sent events;
- a CLI for people and agents (the same binary, or a client-only build).

Adding a channel suggests its name, group, tvg-id and logo. Webhooks (for example Jellyfin's guide
refresh) fire when something changes, and channels are checked through gateways that support it.

## Run it

```sh
docker compose -f deploy/compose.yaml up -d     # ghcr.io/alexey-lapin/lineup on port 9300
brew install alexey-lapin/tap/lineup            # the CLI
```

[docs/deploy.md](docs/deploy.md) goes from there to a channel playing in Jellyfin with a guide.

## Outputs

| URL | What |
|---|---|
| `/` | Web UI |
| `/p/<slug>.m3u` (or `.m3u8`) | M3U playlist. `tvg-chno` is the position in the playlist. |
| `/p/<slug>/epg.xml` | XMLTV guide with just that playlist's channels. |
| `/api/v1/...` | REST API |
| `/api/v1/events` | Server-sent events (`playlist.changed`, `epg.updated`, `channel.unhealthy`, ...) |
| `/api/openapi.json` | OpenAPI document |

## CLI

```sh
lineup route add Streams --gateway http://gateway-host:8099 --rule <rule>
lineup add -r Streams <ref> -p tv --yes     # name, group, tvg-id and logo suggested
lineup add -r Direct https://example.com/live.m3u8 --name "Arena 1" -p tv
lineup ls --playlist tv
lineup info "Arena 1"                       # latest check and what the gateway observes now
```

`lineup --help` lists every command. The server is chosen by `--server`, then `LINEUP_SERVER`, then
`server = "…"` in `~/.config/lineup/config.toml`, then `http://127.0.0.1:9300`. Every command takes
`--json`, and exit codes are stable ([spec §11](docs/spec.md#11-cli)).

## Docs

- [Deploying lineup](docs/deploy.md)
- [lineup-gateway](docs/gateway.md)
- [Spec](docs/spec.md): how lineup behaves, in detail
- [Contributing](CONTRIBUTING.md): development and releases

MIT licensed.

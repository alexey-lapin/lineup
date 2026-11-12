# lineup

lineup manages IPTV channels and playlists, instead of a hand-edited M3U file. It serves each playlist as M3U and its guide as XMLTV over HTTP, for players and media servers that read them from a URL, such as Jellyfin and Emby Live TV, Kodi with IPTV Simple, or IPTV apps like TiviMate. Each channel is played through a route you pick: **direct** (a stream URL, put in playlists as it is) or a **gateway** route, where a separate gateway service turns a reference such as an Acestream id or a video page into a playable URL. lineup itself knows no streaming protocol; gateways speak a small contract (crate `gateway-contract`). One Rust binary serves the web UI (installable on an iPhone home screen), a REST API, the playlists as M3U and a per-playlist XMLTV guide. It downloads guide data itself, suggests names and tvg-ids when you add a channel, calls webhooks (for example Jellyfin's guide refresh) when something changes, and checks channels and shows their live state through gateways that support it. The same binary is also a CLI for people and agents.

- Spec and roadmap: [docs/spec.md](docs/spec.md)
- Deploying: [docs/deploy.md](docs/deploy.md)

## Outputs

| URL | What |
|---|---|
| `/` | Web UI |
| `/p/<slug>.m3u` (or `.m3u8`) | M3U playlist for your player or media server. `tvg-chno` is the position in the playlist. |
| `/p/<slug>/epg.xml` | XMLTV guide with just that playlist's channels. |
| `/api/v1/events` | Server-sent events (`playlist.changed`, `epg.updated`, `channel.unhealthy`, `route.changed`, ...) |
| `/api/v1/...` | REST API |
| `/api/openapi.json` | OpenAPI document |

## CLI

```sh
lineup route add Acestream --gateway http://media-host:8099 --rule acestream   # see: route test --gateway URL
lineup route ls | show <route> | edit <route> [--name --base-url --enable|--disable] | rm <route> | resync <route>
lineup add -r Acestream acestream://<id> -p tv --yes   # name, group, tvg-id and logo suggested from the gateway and guide
lineup add -r Acestream <id> --name "Arena 1" --group Sports --tvg-id Arena.1.de -p tv
lineup add -r VK "https://vk.com/video-1_2?list=..." --name "A vs B" --event --kickoff 2026-11-08T20:00:00Z -p tv
lineup add -r Direct https://example.com/live.m3u8 -p tv
lineup ls [query] [--group G] [--playlist P] [--route R] [--health ok|dead]
lineup show|edit|rm|enable|disable <channel>   # channel = id, exact name, or its stored source/identity
lineup info <channel>                          # latest check and what the gateway observes now
lineup playlist ls|show|create|rename|rm|add|remove|move|set|url
lineup import <file|url|-> -p <slug> -r <route> [--dry-run]
lineup epg sources ls|add|edit|rm ; epg fetch [id...] ; epg search <query>
lineup hook ls|add|edit|enable|disable|rm|test|deliveries
lineup check [channel...] [--force]
lineup settings get | settings set epg_refresh_hours|check_interval_hours <n>
lineup refresh [slug...]
lineup status
lineup backup <file>                            # consistent SQLite snapshot of the server DB
lineup usage                                   # usage spec (https://usage.jdx.dev)
lineup completions <shell>                     # bash, zsh, fish, elvish, powershell
```

Every command accepts `--json`, which prints the API response, and `-q`. The server is chosen by `--server`, then `LINEUP_SERVER`, then `~/.config/lineup/config.toml` (`server = "http://host:9300"`), and finally defaults to `http://127.0.0.1:9300`.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | ok |
| 1 | error |
| 2 | usage or invalid input |
| 3 | not found |
| 4 | conflict, such as a duplicate source |
| 5 | server unreachable |
| 6 | a route's gateway failed or is unreachable |

`add` takes several inputs. With `--json` it always prints an array with one entry per input: `{input, ok, channel | would_create, error: {code, message}}`. It exits 0 when every input succeeds. When every input fails with the same kind of error, it exits with that error's code. Otherwise (partial success or mixed errors) it exits 1.

## Development

```sh
cargo test                                         # server, CLI and HTTP tests (no node needed)
cargo run -p lineup -- serve --data-dir ./data     # http://127.0.0.1:9300
corepack pnpm -C web install && corepack pnpm -C web dev   # UI dev server, proxies /api and /p to :9300
```

After you change the API:

```sh
cargo run -q -p lineup-server --bin openapi > web/src/api/openapi.json
corepack pnpm -C web gen:api
```

CI checks that both generated files are up to date.

Release: push a `v*` tag (a tag with a `-`, like `v1.0.0-rc.1`, becomes a pre-release). The release workflow runs the same checks as CI, builds `ghcr.io/<owner>/lineup` for amd64 and arm64 on native runners and CLI archives for macOS and Linux (x86_64 and arm64), and only then publishes the GitHub release with the archives, `SHA256SUMS` and notes. Notes list merged pull requests by label (`.github/release.yml`), or the commits since the previous tag when there are none (`.github/scripts/release-notes.sh`). To rebuild an existing tag, run the workflow by hand with that tag; tick `cli` to rebuild the archives and refresh the release files too.

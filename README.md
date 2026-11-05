# lineup

lineup manages channels and playlists for Jellyfin Live TV, instead of a hand-edited M3U file. Channels are Acestream streams (played through [acexy](https://github.com/Javinator9889/acexy)), live VK videos (played through a VK proxy), or plain stream URLs. One Rust binary serves the web UI (installable on an iPhone home screen), a REST API, and the playlists as M3U. The same binary is also a CLI for people and agents.

- Spec and roadmap: [docs/spec.md](docs/spec.md)
- Deploying: [docs/deploy.md](docs/deploy.md)

## Outputs

| URL | What |
|---|---|
| `/` | Web UI |
| `/p/<slug>.m3u` (or `.m3u8`) | Playlist for Jellyfin's M3U tuner. `tvg-chno` is the position in the playlist. |
| `/api/v1/...` | REST API |
| `/api/openapi.json` | OpenAPI document |

## CLI

```sh
lineup add acestream://<id> --name "Arena 1" --group Sports --tvg-id Arena.1.de -p jellyfin
lineup add "https://vk.com/video-1_2?list=..." --name "VK Live - A vs B" --event --kickoff 2026-11-08T20:00:00Z -p jellyfin
lineup ls [query] [--group G] [--playlist P] [--kind acestream|vk|raw]
lineup show|edit|rm|enable|disable <channel>   # channel = id, exact name, or any source input
lineup playlist ls|show|create|rename|rm|add|remove|move|set|url
lineup import <file|url|-> -p <slug> [--dry-run]
lineup settings get | settings set <key> <value>
lineup refresh [slug...]
lineup status
lineup backup <file>                            # consistent SQLite snapshot of the server DB
lineup usage                                   # usage spec (https://usage.jdx.dev)
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

Release: push a `v*` tag. The release workflow first runs the same checks as CI, then builds `ghcr.io/<owner>/lineup` for amd64 and arm64, plus CLI tarballs for macOS and Linux.

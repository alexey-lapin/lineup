# lineup — spec

## 1. Problem

Media servers (Jellyfin Live TV, Plex, Emby, Channels DVR) and IPTV players read channels from an M3U playlist and guide data from XMLTV, but the playlist itself is usually a hand-edited file. lineup started as a replacement for one such file behind Jellyfin. When the channels are Acestream streams (played through [acexy](https://github.com/Javinator9889/acexy)) and live VK videos (played through a VK proxy), that file gets messy fast. The host address is repeated on every line, tvg-ids are matched to the guide by hand, and any script that builds the guide has to grep the file.

lineup is a standalone service that owns the channels. It serves playlists and their EPG over HTTP to any client that reads them from a URL, and tells Jellyfin (or any webhook) when they change. You drive it from a web UI (an iOS home-screen app) or from a CLI that agents can use too.

## 2. Goals and non-goals

**Goals (v1)**
- Channel library with typed sources and base URLs configured once.
- Named playlists with channels picked by hand, served as `/p/<slug>.m3u` with a per-playlist `/p/<slug>/epg.xml`.
- The app fetches the EPG itself.
- An add flow that suggests a name and matching tvg-id and logo.
- Events → hooks (a Jellyfin RefreshGuide hook and generic webhooks).
- Health probes for acestream channels (status only).
- A Vue + Nuxt UI PWA for managing, and a Rust CLI over the same REST API.

**Non-goals (v1)**
- Relaying or transcoding video. Clients stream directly from acexy and the VK proxy.
- In-app playback.
- Auth: lineup is meant for a trusted LAN.
- Change history or revert, and validation beyond parsing input.
- Expiry for VK events (they stay until removed).
- Placeholder EPG for channels other than VK events.
- Rule-based playlist membership.

## 3. Deployment

- One Rust binary. The built UI is embedded with rust-embed. Everything is served on one port: UI, `/api/*`, `/p/*`, and `/healthz`.
- Docker image built by GitHub Actions and pushed to GHCR with pinned semver tags. macOS and Linux CLI binaries are attached to GitHub releases.
- Boot configuration comes only from env:
  - `LINEUP_BIND` (default `0.0.0.0:9300`)
  - `LINEUP_DATA_DIR` (default `/data`)
  - `LINEUP_LOG`
- Everything else is runtime settings stored in SQLite.
- Every schedule is boot-relative (run shortly after start, then every N hours), so hosts that sleep or power off overnight still get fresh data soon after they come back.

## 4. Data model (SQLite via sqlx, with migrations)

```
settings(key TEXT PK, value JSON)          -- base URLs, EPG schedule, probe schedule
channels(
  id INTEGER PK, name TEXT, group_title TEXT NULL,
  tvg_id TEXT NULL, tvg_logo TEXT NULL,
  source_kind TEXT CHECK IN ('acestream','vk','raw'),
  source JSON,          -- acestream: {id:"<40hex>"}; vk: {owner_id:-100200300, video_id:456000001, list:"4d2c…"|null}; raw: {url}
  source_key TEXT NOT NULL,   -- computed in Rust on write, see below
  is_event BOOL DEFAULT false, kickoff_at DATETIME NULL,   -- VK events
  enabled BOOL DEFAULT true,
  created_at, updated_at,
  UNIQUE(source_kind, source_key)
)
channel_health(channel_id PK REFERENCES channels ON DELETE CASCADE,
  conclusive TEXT NULL ('ok','dead'), conclusive_at,          -- last conclusive result; drives events + badge
  last_attempt TEXT ('ok','dead','unknown','skipped'), last_attempt_at, detail JSON)
playlists(id INTEGER PK, slug TEXT UNIQUE, name TEXT, created_at, updated_at)
playlist_items(playlist_id REFERENCES playlists ON DELETE CASCADE,
  channel_id REFERENCES channels ON DELETE CASCADE,
  position INTEGER NOT NULL, PRIMARY KEY(playlist_id, channel_id), UNIQUE(playlist_id, position))
epg_sources(id INTEGER PK, name TEXT, url TEXT, priority INTEGER, enabled BOOL,
  current_gen INTEGER NULL, previous_gen INTEGER NULL, last_fetch_at, last_status, last_error)   -- validators live in the generation dir (§6)
epg_channels(source_id, xmltv_id TEXT, display_names JSON, icon TEXT NULL)   -- index used for matching and suggestions
hooks(id INTEGER PK, name, kind ('jellyfin','webhook'), config JSON, events JSON, debounce_secs INT, enabled BOOL)
hook_deliveries(id, hook_id, event, status, http_code, error, at)   -- ring buffer, last ~200 per hook
```

**URL building**
- Settings hold `acexy_base` (e.g. `http://media-host:8080`), `engine_base` (`http://media-host:6878`) and `vk_base` (`http://media-host:8099`).
- acestream → `{acexy_base}/ace/getstream?id={id}`
- vk → `{vk_base}/vk/{owner_id}_{video_id}`, plus `?list={list}` only when `list` is set. If it is null, the query is left out entirely.
- raw → the URL as stored.

**Source keys** (used for de-duplication and import matching):
- acestream: the 40-hex id, lowercased.
- vk: `"{owner_id}_{video_id}"` with signed integers. `list` is not part of the key.
- raw: the URL exactly as given, after trimming whitespace. No normalization.

**Positions**
- `position` counts from 1 and is emitted as `tvg-chno`.
- Every mutation keeps positions dense (1..n), and disabled channels keep their slot. So disabling a channel never renumbers the others, but delete, insert and move do.
- Insert at N shifts the following items. Remove compacts. A full-order replace (`PUT items`) renumbers 1..n in the given order.
- Because `UNIQUE(playlist_id, position)` would conflict partway through a reorder, reorders first move the affected rows to negative positions and then write the final ones.
- Test case: in [A,B,C], moving C to 1 gives [C,A,B]. Swapping A and B with a full replace gives [B,A,C].
- All of these run in one transaction.

**Effective tvg-id**
- `effective_tvg_id = tvg_id ?? "lineup.<channel_id>"`.
- Used the same way in both the M3U and the XMLTV.

## 5. Outputs

### `/p/<slug>.m3u`
```
#EXTM3U url-tvg="http://<host>/p/<slug>/epg.xml"
#EXTINF:-1 tvg-id="Arena.1.de" tvg-chno="2" tvg-logo="…" group-title="Sports",Arena 1
http://media-host:8080/ace/getstream?id=816b…
```
- Built per request from the DB, which is cheap. The `ETag` is a hash of the rendered body, so base-URL changes invalidate it automatically.
- Channels use the effective tvg-id (§4).
- `.m3u8` is also accepted as an alias.

### `/p/<slug>/epg.xml`

Built per request from the per-source caches. Output order is all `<channel>` elements first, then all `<programme>` elements, as the XMLTV DTD requires.

**Upstream channels**
- For each non-event channel in the playlist, take the matching `<channel>` and its programmes from the source caches.
- When the same id appears in several sources, the source with the highest priority wins, and only that source's programmes are used. No programme deduplication.

**VK event channels**
- Always emit a synthetic `<channel id="{effective_tvg_id}">` with the channel name as display-name. If an upstream source happens to carry the same id, the synthetic one replaces it.
- Programmes, relative to `now` truncated to the hour, covering now to +24h:
  - With `kickoff_at`: the logical blocks are "Pre-match" [−∞, kickoff), the title [kickoff, kickoff+2h), and the title marked "(ended)" [kickoff+2h, +∞).
  - Each block is clipped to the window [hour, hour+24h). Empty intersections are dropped.
  - So a kickoff 48h away gives a single Pre-match block, and an event that already ended gives a single "(ended)" block starting at the current hour.
  - Without kickoff: the title in a rolling block from now to +24h.

**Caching**
- The response is cached in memory, keyed by (playlist, epg generation, current hour).
- It is invalidated by EPG publish, output-affecting mutations (§8), or the hour boundary, so placeholders stay current even if upstream fetches keep failing.

## 6. EPG ingestion

- `epg_sources` starts empty. You add XMLTV URLs (plain or gzipped), for example regional files from a public EPG provider.
- Schedule: 3 minutes after start, then every `epg_refresh_hours` (default 6; 0 turns it off). The scheduler compares wall-clock time every 10 minutes, so a host that slept catches up soon after it wakes. A fetch can also be started from the UI or CLI, and adding a source or changing its URL fetches it right away.
- The **unfiltered raw download is kept**. A regional guide is tens of MB gzipped, which is fine. Because filtering happens when the response is served (§5), adding a tvg-id never needs a re-download.

**Generations.** Each source has a directory `/data/epg/<source_id>/`, and each successful download becomes a generation directory `gen-<n>/` inside it. A generation holds:
- `raw`: the download as received, plain or gzipped (detected by its magic bytes).
- `programmes.xml`: each kept `<programme>` element, re-emitted from the source event by event so no metadata is dropped, grouped by channel and sorted by start.
- `programmes.idx`: for each channel id, the byte range of its block in `programmes.xml`.
- `channels.json`: the `<channel>` elements plus display names and icon. Ids that only appear in programmes get an entry without an element.
- `meta.json`: `{url, etag, last_modified, fetched_at}`. These are the validators returned with this download, together with the URL they came from.
- `COMPLETE`: a marker file written last.

A download is accepted only if it parses as a whole document, including the closing `</tv>`, so a truncated transfer never replaces good data.

`epg_sources.current_gen` points at the generation being served. Validators are read from the current generation's directory, never stored separately.

**Fetch sequence**
1. Send validators only when the current generation is complete *and* its `meta.url` equals the source's configured URL. Otherwise send none. So after a URL change, every attempt stays unconditional until a generation for the new URL is published, including across restarts.
2. On `304`: if the current generation is complete, there is nothing to do. If it is incomplete or missing, rebuild it from the raw file inside it; if that fails, fetch again unconditionally.
3. On `200`:
   1. Stream the body into `gen-<n+1>/raw` (at most 1 GiB).
   2. Parse it and write `programmes.*` and `channels.json`, dropping programmes that ended more than 6h ago.
   3. Write `meta.json`, then fsync, then write `COMPLETE`.
   - `n+1` is always max(existing dirs)+1, so a leftover complete candidate is never overwritten in place.
   - Fetch and publish for a source run in a single worker per source (or under a per-source lock).
4. **Publish**, in one DB transaction: set `previous_gen = current_gen`, `current_gen = n+1` and replace this source's `epg_channels` rows. Then invalidate the EPG response cache and emit `epg.updated`.
5. Afterwards, delete generations older than the previous one.

**Failures and startup**
- Any error before publish leaves `current_gen` and the old directory untouched, and is recorded in `last_status` / `last_error`.
- On startup, generation directories without `COMPLETE` are deleted.
- If `current_gen` points at a directory that is incomplete or missing, the app falls back to the newest complete generation that was *previously published*, which is `current_gen` or `previous_gen`, and republishes it (setting `current_gen` and rebuilding `epg_channels`) in one transaction. If there is none, the source has no data and its next fetch is unconditional.
- Complete generations that were never published (a crash between `COMPLETE` and the commit) are not promoted automatically; they are cleaned up by step 5.
- epg.xml and suggestions only read published generations of *enabled* sources.

## 7. Add flow and suggestions

`POST /api/resolve {input}` accepts:
- `acestream://<id>`, a bare 40-hex id, or an acexy or engine URL containing `id=`.
- A vk.com video URL in any form: `vk.com/video-OWNER_VIDEO?list=…`, `vkvideo.ru/…`, or `vk.com/…?z=video-OWNER_VIDEO%2F…`.
- The VK proxy URL.
- Any other http(s) URL, which becomes raw.

It returns the parsed source and a duplicate check (an existing channel with the same source key). For acestream it also looks up the engine name:
- `GET {engine_base}/server/api?method=get_media_files&content_id=<id>`, e.g. `"Arena 1 Bar HD [DE]"`.
- Name cleanup: remove `[XX]` country tags, `HD`/`FHD`/`UHD`/`Bar` and similar quality words, and extra spaces. The country tag becomes a region hint.
- Fuzzy match the cleaned name against `epg_channels.display_names`, using normalized token similarity (strsim), with a bonus for a matching region suffix (`.de`, `.uk`, `.us`).
- Return the top 5 `{xmltv_id, display_name, icon, score}`.

Final suggestions: `name` from the cleaned engine name, `tvg_id` and `tvg_logo` from the best match when it scores at least 0.6, and `group_title` from a heuristic (acestream → "Sports" if the name looks like sports; vk → "VK", also marked as an event). Engine failures are reported as warnings and never fail the resolve. The UI fills empty fields with the suggestions and offers the other matches and a guide search. In the CLI, the suggested name is always used unless `--name` is given; the other suggestions are applied with `--yes`, or after a confirmation prompt on a terminal. Explicit flags always win.

## 8. Events and hooks

**Events**
- `playlist.changed {slugs}`
- `epg.updated {sources?}`: guide content may have changed. Emitted by source publishes, source config changes, and event-channel guide changes.
- `channel.unhealthy {channel_id, name, detail}`
- `channel.recovered {channel_id, name}`

**Which mutations affect which outputs.** Every write goes through one service function. It computes the affected slugs *before* deleting anything, then invalidates caches and emits events.

| Mutation | Affected playlists | Emits |
|---|---|---|
| channel name / group / tvg_id / logo / source / enabled / is_event / kickoff | all containing the channel | `playlist.changed` (+ `epg.updated` if tvg_id / is_event / kickoff, or an event channel's name, changed) |
| channel delete | all that contained it (captured before delete) | `playlist.changed` |
| item add / remove / move / replace | that playlist | `playlist.changed` |
| playlist create / rename / delete | that slug | `playlist.changed` |
| `acexy_base` / `vk_base` change | playlists with ≥1 channel of that kind | `playlist.changed` |
| EPG source publish | all | `epg.updated` |
| EPG source enable / disable / delete / priority change | all (EPG cache flushed) | `epg.updated` |
| EPG source URL change | — (current gen stays served until the new URL's first successful publish; fetches stay unconditional until then per §6, first one runs immediately) | `epg.updated` on that publish |
| health status transition | — | `channel.unhealthy` / `channel.recovered`, SSE only |
| health check with no transition | — | SSE only |

**Dispatcher**
- One tokio task per hook, with a trailing-edge debounce of `debounce_secs` (default 30). A window never stays open longer than four debounce periods, so a steady stream of changes still gets delivered.
- The delivered body is the envelope `{events: [{type, at, payload}…], window_start, window_end}`, so mixed events keep their own payloads.
- Retries 3 times with backoff (2s, 10s, 30s). Each attempt is logged; the last 200 rows per hook are kept.
- A 2xx means *accepted*, not that the downstream is fresh.

**Hook kinds**
- `jellyfin`: `{base_url, api_key, task_id?, followup_minutes: 65}`.
  - Calls `POST {base}/ScheduledTasks/Running/{task_id}` with `Authorization: MediaBrowser Token="…"`. If `task_id` is empty, it finds the task by key `RefreshGuide` via `GET /ScheduledTasks`.
  - Never restarts Jellyfin.
  - **Freshness**: Jellyfin's XMLTV provider reuses its downloaded XML for about 1h. Any change to a playlist's membership or channels can change its epg.xml. So *every* delivery (`playlist.changed` or `epg.updated`) also schedules a single follow-up RefreshGuide `followup_minutes` later. If more triggers arrive, that one follow-up is moved later rather than adding another. The follow-up call itself never schedules another follow-up.
  - Contract: channel-list changes appear within about debounce plus refresh time. Guide-data changes appear within about 65 min of the *latest* merged guide change, assuming Jellyfin is up (later triggers move the follow-up). Verify the cache window against current Jellyfin releases (§15).
- `webhook`: `{method, url, headers{}}`. The body is always the envelope above. Body templates are deferred.
- Every hook has a "test" action that sends a synthetic event.
- Secrets (api_key, header values) are write-only in the API and masked in responses.

## 9. Health probes (acestream only)

This measures **swarm health**, not a guarantee that playback works.

**Probe**
1. `GET {engine_base}/ace/getstream?id=<id>&format=json&pid=lineup-probe-<uuid>` (a distinct pid per probe). Parse the `{response: {stat_url, command_url, …}, error}` envelope.
2. The returned URLs point at the engine's own loopback address. **Rebase them** onto `engine_base`, keeping path and query.
3. Poll `stat_url` every 2s for up to 20s. Every request has a 5s timeout.
4. Result: `ok` if `peers > 0` or `status == "dl"`, `dead` if the session started but stayed at `prebuf` with 0 peers. Any engine or network failure is `unknown`, with the detail recorded.
5. Always send `command_url?method=stop` for the session this probe created, and only that one, with a 5s timeout.

**Activity guard (periodic runs)**
- Before *each* probe, check acexy status.
  - If a per-id status is available (`/ace/status?id=`, present in recent acexy versions), skip only that id. A 404 or "not found" there means the id is *inactive*, so the probe goes ahead. Only transport errors and 5xx count as unreadable.
  - Otherwise, if the global `streams > 0`, stop the run and retry in 30 min.
- If status can't be read, stop the run (fail closed).
- Skipped channels are recorded as `last_attempt = skipped`.
- Viewers can still start between checks. This is best effort; check concurrent playback behaviour before turning the schedule on.

**Schedule**
- 10 min after start, then every `probe_interval_hours` (default 0, which is **off**). A run stopped by the guard is retried after 30 min.
- One channel at a time.
- Manual probes from the UI or CLI (`POST /probe {ids?: [], force: bool}`) apply the same guard unless `force` is set. With ids the request waits and returns the results; without, the run continues in the background (202).
- An acexy per-id status that is a 200 is read for a client count (`clients`, `streams`, `viewers` or `active`); when it has none, the id is skipped rather than risk interrupting a viewer.

**Results**
- Results only write `channel_health`.
  - Every attempt updates the `last_attempt*` fields.
  - Only `ok` and `dead` results update `conclusive`.
- Events compare the new conclusive result with the previous `conclusive`:
  - `null|ok → dead` emits `channel.unhealthy`. The first result being dead also counts.
  - `dead → ok` emits `channel.recovered`.
  - No change emits nothing.
- `unknown` and `skipped` never emit and never touch `conclusive`, so "dead, unknown, skipped, ok" emits exactly one recovery.
- The UI badge shows `conclusive`, with a hint when the latest attempt is inconclusive.
- Playlists are never edited.

## 10. REST API (`/api/v1`, JSON, OpenAPI via utoipa at `/api/openapi.json`)

```
GET    /channels?group=&q=&health=          POST /channels          GET/PATCH/DELETE /channels/{id}
POST   /resolve                              POST /probe {ids?: [id], force?: bool}   (no ids = all acestream)
POST   /refresh {slugs?: []}   -- emits playlist.changed for the given playlists (all if omitted)
GET    /playlists                            POST /playlists         GET/PATCH/DELETE /playlists/{slug}
PUT    /playlists/{slug}/items  {channel_ids: [id,…]}  (full membership + order replace; returns the playlist)
POST   /playlists/{slug}/items  {channel_id, position?}   DELETE /playlists/{slug}/items/{channel_id}  (both return the playlist)
POST   /playlists/{slug}/items/{channel_id}/move {position}
       Channel/playlist DELETE return 204. Import-by-URL bodies are capped at 2 MiB.
GET    /epg/sources  POST/PATCH/DELETE …     POST /epg/fetch         GET /epg/channels?q=  (search for tvg-id picker)
GET    /hooks  POST/PATCH/DELETE …           POST /hooks/{id}/test   GET /hooks/{id}/deliveries
GET/PATCH /settings
GET    /backup   -- consistent SQLite snapshot (VACUUM INTO); not in OpenAPI
POST   /import  {m3u_text | url, playlist_slug, dry_run}
       dry_run: strictly read-only; returns per-entry {parsed channel, kind, source_key, action: create|reuse|skip_in_playlist, diff vs existing}
       commit: one transaction. Existing channel with same source key is reused as-is (metadata NOT overwritten; diff reported);
               new ones created; missing members appended to the playlist in file order; playlist created if absent.
               Emits playlist.changed once. Returns the same per-entry report.
GET    /events (SSE)   -- live updates for the UI: channel/playlist changes, probe results, fetch progress
```
- Errors use `{error: {code, message}}` with stable `code` strings, so the CLI can map them to exit codes.

## 11. CLI

- The same binary: `lineup serve` runs the server, every other subcommand is a client.
- Server address: `--server` / `LINEUP_SERVER` / `~/.config/lineup/config.toml`.
- Built with clap derive. `clap_usage` produces a jdx usage spec (`lineup usage`), which drives completions and docs. `--json` on every command, plus `-q`.
- Exit codes: 0 ok, 1 generic, 2 usage, 3 not found, 4 conflict/duplicate, 5 server unreachable.

```
serve
add <input>... [--name --group --tvg-id --logo --playlist <slug>] [--event --kickoff <time>] [--yes] [--dry-run]
ls [--group --playlist --health dead] ; show <id> ; edit <id> [--name …] ; rm <id> ; enable/disable <id>
playlist ls|create <slug> [--name]|rm <slug>|show <slug>
playlist add <slug> <channel>... [--at N] ; playlist remove <slug> <channel>... ; playlist move <slug> <channel> <pos>
playlist url <slug>     # prints m3u + epg URLs
epg fetch ; epg sources ls|add|rm ; epg search <query>
probe [<id>...] [--force]     # POST /probe
hook ls|add|rm|test <id> ; hook deliveries <id>
refresh [<slug>...]           # POST /refresh
import <file|url> --playlist <slug> [--dry-run]
settings get|set <key> <value>
backup <file>
usage                   # emit usage spec
```
- `<channel>` accepts an id, an exact name, or an input that resolves to an existing source, such as an ace id.

## 12. Web UI (Vue 3 + Vite + TS + @nuxt/ui v4 in Vue mode, vue-router, Pinia Colada; a hand-written manifest and service worker)

- **Channels**
  - Searchable table with group, health badge, source kind and playlist chips.
  - An "Add" slideover where you paste the input and see the suggestions with editable fields: tvg-id picker backed by `/epg/channels`, logo preview, playlist checkboxes.
  - Row actions: probe, copy stream URL, open in VLC (`vlc-x-callback://x-callback-url/stream?url=…`) or Infuse (`infuse://x-callback-url/play?url=…`), edit, delete.
- **Playlists**
  - A list, plus an editor with drag-to-reorder (position = chno) and add/remove channels.
  - Copy buttons for the m3u and epg URLs.
- **EPG**: sources with last fetch status, a "fetch now" button, and a channel search.
- **Hooks**: CRUD, test button, delivery log.
- **Settings**: base URLs (with a "test" ping) and schedules.
- **Import**: paste or upload an m3u, preview, confirm.
- **Live updates**: SSE invalidates queries.
- **PWA**: manifest, icons, `apple-mobile-web-app-capable`, safe-area insets, mobile-first layout using UDashboard* components that collapse to a bottom nav or drawer on phones.
  - Caveat: over plain HTTP on a LAN the service worker won't register on iOS. It still works as a home-screen web app, just with no offline support, which is acceptable.
- **Types**: TS types are generated from OpenAPI (`openapi-typescript`) and called through a thin `openapi-fetch` client.

## 13. Repo layout

```
Cargo.toml (workspace)
crates/
  server/        axum app, db, epg, hooks, probe, m3u, resolve
  cli/           clap client (depends on api-types)
  api-types/     shared serde DTOs + utoipa schemas
  lineup/        the binary: dispatches `serve` → server, else → cli
web/             Vue app (pnpm), builds to web/dist, embedded by server via rust-embed
docs/            spec.md, deploy.md
Dockerfile       multi-stage: node build web → rust build (musl) → alpine
.github/workflows/  ci.yml (fmt, clippy, test, web typecheck/build/test), release.yml (GHCR + binaries)
deploy/          example compose.yaml, restore.sh
```

## 14. Milestones

**Build order** (CI and Dockerfile):
1. `cargo run -p lineup-server --bin openapi > web/src/api/openapi.json`. The bin only builds the router metadata and needs no running server.
2. `pnpm gen:api && pnpm build`, which outputs `web/dist`.
3. `cargo build --release`. rust-embed reads `web/dist` relative to the server crate. `build.rs` writes a placeholder `web/dist/index.html` when it is missing, so `cargo check` works without node.
4. sqlx uses runtime queries, not `query!` macros, so there is no `.sqlx/` offline data and the build needs no database. Every query is covered by tests.

**M1 Core + deployable** (done).
- Workspace, migrations, the single mutation path from §8 (events without consumers yet), channels/playlists CRUD, URL building, `/p/<slug>.m3u`, import with dry-run and commit, `refresh`, backup.
- CLI for all of these, a minimal UI (channels, playlists, import, settings), the Dockerfile, CI, and GHCR releases.
- Usable on its own: point Jellyfin's M3U tuner at `/p/<slug>.m3u` and keep the existing guide source.

**M2 EPG + add flow** (done).
- Sources, scheduled fetch with raw retention and indexes, per-playlist epg.xml, VK placeholders, resolve/suggestions, the add flow in UI and CLI.
- Jellyfin setup doc: one XMLTV provider pointing at `/p/<slug>/epg.xml`. Note that Jellyfin derives channel ids partly from the tuner URL, so switching an existing tuner to lineup can reset favourites.

**M3 Hooks + health** (done).
- Event bus consumers, Jellyfin hook (with follow-up) and webhook hooks, delivery log, SSE.
- Probes, with the schedule off by default until the engine response and acexy status have been captured as test fixtures.

**M4 Polish** (done).
- PWA manifest, icons and mobile layout. The service worker caches only the UI shell (never `/api` or `/p`), and is registered only in a secure context.
- Open-in-player links, `usage` spec and `completions`, CLI release binaries for macOS and Linux on x86_64 and arm64, deploy docs.

## 15. Open questions

- Which acexy versions support `/ace/status?id=`? Without it, the global guard applies.
- The exact engine `format=json` response (the envelope, and the loopback host in returned URLs). Capture as test fixtures before M3.
- Does every VK proxy need the `list` hash, or is it optional?
- Does RefreshGuide re-download an M3U from a URL tuner, and how long is Jellyfin's XMLTV cache window (about 1h upstream)? Measure in M2 and adjust `followup_minutes`.

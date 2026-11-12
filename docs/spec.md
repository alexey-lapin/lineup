# lineup — spec

## 1. Problem

Media servers (such as Jellyfin and Emby Live TV) and IPTV players read channels from an M3U playlist and guide data from XMLTV, but the playlist itself is usually a hand-edited file. lineup started as a replacement for one such file behind Jellyfin. When channels come from several places (Acestream streams played through [acexy](https://github.com/Javinator9889/acexy), live video pages that need a lookup before they play, plain stream URLs), that file gets messy fast. The host address is repeated on every line, tvg-ids are matched to the guide by hand, and any script that builds the guide has to grep the file.

lineup is a standalone service that owns the channels. It serves playlists and their EPG over HTTP to any client that reads them from a URL, and tells Jellyfin (or any webhook) when they change. You drive it from a web UI (an iOS home-screen app) or from a CLI that agents can use too.

lineup itself knows no streaming protocol. A channel is played through a **route**: either *direct* (the source is a stream URL, emitted as it is) or a **gateway** (a separate service that turns a reference, such as an Acestream id or a video page, into a playable URL). The gateway contract is in crate `gateway-contract`; the first gateway is lineup-gateway.

## 2. Goals and non-goals

**Goals**
- A channel library where each channel is played through a route you pick (direct, or a gateway and one of its rules).
- Named playlists with channels picked by hand, served as `/p/<slug>.m3u` with a per-playlist `/p/<slug>/epg.xml`.
- The app fetches the EPG itself.
- An add flow that suggests a name, group and matching tvg-id and logo, from the gateway's hints and the guide.
- Events → webhooks (e.g. Jellyfin's RefreshGuide task).
- Channel checks through gateways that support them (status only).
- A Vue + Nuxt UI PWA for managing, and a Rust CLI over the same REST API.

**Non-goals**
- Relaying or transcoding video in lineup. Players stream from the route's target: the direct URL, the gateway, or the service the gateway points at.
- Knowledge of any streaming protocol or site in lineup; that lives in gateways.
- In-app playback.
- Auth: lineup is meant for a trusted LAN.
- Change history or revert.
- Expiry for event channels (they stay until removed).
- Placeholder EPG for channels other than event channels.
- Rule-based playlist membership.
- Custom URL templates (direct routes only take whole URLs).

## 3. Deployment

- One Rust binary. The built UI is embedded with rust-embed. Everything is served on one port: UI, `/api/*`, `/p/*`, and `/healthz`.
- Docker image built by GitHub Actions and pushed to GHCR with pinned semver tags. macOS and Linux CLI binaries are attached to GitHub releases.
- Boot configuration comes only from env:
  - `LINEUP_BIND` (default `0.0.0.0:9300`)
  - `LINEUP_DATA_DIR` (default `/data`)
  - `LINEUP_LOG`
- Everything else is runtime configuration stored in SQLite: routes (§4b) and settings.
- Every schedule is boot-relative (run shortly after start, then every N hours), so hosts that sleep or power off overnight still get fresh data soon after they come back.
- lineup works without any gateway (direct routes). Gateway routes need their gateway reachable to add, edit, import or resync channels; playlists and guides never call a gateway.

## 4. Data model (SQLite via sqlx, with migrations)

0.5.0 started a new schema lineage (one migration 0001). lineup identifies a database's lineage by the
checksum of its applied migration 1: a database of lineup 0.4 or older (or one with tables but no
migration history) is refused at startup, untouched, before anything is written to it. Later
migrations of the 0.5 lineage are handled by the migrator as usual.

```
settings(key TEXT PK, value JSON)          -- epg_refresh_hours, check_interval_hours, revision
routes(
  id INTEGER PK AUTOINCREMENT, name TEXT UNIQUE,
  kind TEXT ('direct','gateway'), base_url TEXT NULL, rule TEXT NULL,   -- gateway only; kind and rule never change
  enabled BOOL, revision INT,
  synced_match_revision TEXT NULL, synced_play_revision TEXT NULL,       -- the gateway rule's revisions at the last complete resync
  last_resync JSON NULL, created_at, updated_at)
channels(
  id INTEGER PK, name TEXT, group_title TEXT NULL,
  tvg_id TEXT NULL, tvg_logo TEXT NULL,
  route_id REFERENCES routes ON DELETE RESTRICT,
  source TEXT,          -- canonical reference from the route (direct: the URL; gateway: the rule's `ref`)
  identity TEXT,        -- de-duplication key within the route
  play TEXT NULL,       -- gateway play reference: base-relative path or absolute http(s) URL
  play_revision TEXT NULL,   -- the rule's play revision when `play` was obtained
  source_revision INT,       -- bumped on every change of route/source/identity/play
  is_event BOOL DEFAULT false, kickoff_at DATETIME NULL,
  enabled BOOL DEFAULT true,
  created_at, updated_at,
  UNIQUE(route_id, identity)
)
channel_health(channel_id PK REFERENCES channels ON DELETE CASCADE,
  conclusive TEXT NULL ('ok','dead'), conclusive_at,          -- last conclusive result; drives events + badge
  last_attempt TEXT ('ok','dead','unknown','skipped'), last_attempt_at,
  content_state TEXT NULL ('live','ended_processing','recording'), reason TEXT NULL, detail JSON)
playlists(id INTEGER PK, slug TEXT UNIQUE, name TEXT, created_at, updated_at)
playlist_items(playlist_id REFERENCES playlists ON DELETE CASCADE,
  channel_id REFERENCES channels ON DELETE CASCADE,
  position INTEGER NOT NULL, PRIMARY KEY(playlist_id, channel_id), UNIQUE(playlist_id, position))
epg_sources(id INTEGER PK, name TEXT, url TEXT, priority INTEGER, enabled BOOL,
  current_gen INTEGER NULL, previous_gen INTEGER NULL, last_fetch_at, last_status, last_error)   -- validators live in the generation dir (§6)
epg_channels(source_id, xmltv_id TEXT, display_names JSON, icon TEXT NULL)   -- index used for matching and suggestions
hooks(id INTEGER PK, name, kind ('webhook'), config JSON, events JSON, debounce_secs INT, enabled BOOL)
hook_deliveries(id, hook_id, events, trigger ('events','test'), attempt, ok, http_code, error, at)   -- last 200 per hook
```

A new database has one route, **Direct**.

**Play URLs** (computed when channels are read; playlists never call a gateway)
- Direct route → the source URL as stored.
- Gateway route → `play` when it is an absolute URL; otherwise `base_url` (trailing slashes trimmed) + `/` + `play`. A relative play path is appended below the base path, never resolved per RFC 3986, so a reverse-proxy prefix such as `http://proxy/gateway` is kept.

**Identity and source**
- Direct: both are the input URL, trimmed. Nothing else is normalized; the query is kept as it is.
- Gateway: the rule decides. The identity is for de-duplication only (VK: `vk:<owner>_<video>`, without the `list` token); the source is self-contained, so the gateway can play, observe or check it after a restart (VK: the canonical vk.com URL with `list`).

**Positions**
- `position` counts from 1 and is emitted as `tvg-chno`.
- Every mutation keeps positions dense (1..n), and disabled channels keep their slot. So disabling a channel (or its route) never renumbers the others, but delete, insert and move do.
- Insert at N shifts the following items. Remove compacts. A full-order replace (`PUT items`) renumbers 1..n in the given order.
- Because `UNIQUE(playlist_id, position)` would conflict partway through a reorder, reorders first move the affected rows to negative positions and then write the final ones.
- Test case: in [A,B,C], moving C to 1 gives [C,A,B]. Swapping A and B with a full replace gives [B,A,C].
- All of these run in one transaction.

**Effective tvg-id**
- `effective_tvg_id = tvg_id ?? "lineup.<channel_id>"`.
- Used the same way in both the M3U and the XMLTV.

## 4b. Routes and gateways

A route is a configured way to play channels; every channel has exactly one.

| Kind | Config | Input it takes |
|---|---|---|
| direct | nothing | an http(s) URL with a host |
| gateway | `base_url` (as players reach it), `rule` (one of the gateway's rules) | whatever the rule accepts (e.g. an Acestream id, a VK video link) |

- Several routes may use the same gateway (e.g. "Acestream" with rule `acestream` and "VK" with rule `vk`). A route's kind and rule never change; create another route instead.
- **Enabled** is the only switch. A disabled route's channels stay listed with their playlist memberships and positions, but are left out of M3U and XMLTV output. Gateway reachability never changes output: a gateway that is down only fails writes and checks.
- A route used by channels can't be deleted (409).
- Each route has a `revision`, raised on every change; of two copies the higher is newer. Writes validated against a route at one revision are refused (409) if it changed meanwhile.

**Validation** (one path for create, source edits, moves between routes, import and resync; clients never send identities or play references):
- Direct: trim; require http(s) and a host.
- Gateway: `GET {base}/v1/info` (refused unless it speaks contract 1 and has the rule), then `POST {base}/v1/match {rule, inputs}` in batches of 500. Per input: `matched` (identity, ref, play, hints, warnings), `no_match` (422 with the reason), or `error` (422 with the gateway's message). An HTTP-level failure fails the whole operation with 502 and code `gateway_unreachable`, `gateway_error` or `contract_mismatch`.
- Gateway calls run before the store's write lock; the write then checks the route revision (and, for edits, the channel's `source_revision`).

**Base URL edits and resync**
- A base URL edit revalidates every channel of the route against the new base first; the new base and every revalidated row are published in one transaction. If any channel fails, the edit is refused (409, listing them) and nothing changes.
- `POST /routes/{id}/resync` revalidates every channel with the current config and publishes the results in one transaction: rows the gateway now answers differently are updated; rows that fail keep their values and are listed; rows whose channel changed during the resync are skipped and listed; identity collisions are never merged; names, groups, tvg-ids and other metadata are never touched; health of a channel whose source changed is cleared. The synced revisions advance only when every channel succeeded. The report is kept as `last_resync`.
- A poller reads `/v1/info` of every enabled gateway route every 10 minutes (first after 1 minute) and resyncs a route whose rule's `match_revision` or `play_revision` differs from the synced ones.
- Play references stay valid within contract 1 (gateways only add play path formats). Changing an absolute target, such as acexy's public address, is an operator migration: keep the old address reachable until every affected route shows no channels pending (`GET /routes/{id}/status`), then retire it.

**Live details** (`GET /channels/{id}/info`): the latest check, and for gateway routes whose rule supports `observe`, what the gateway reports now (`/v1/observe`): content state, whether someone is watching, labelled facts, a title. Observing never starts a stream and is never recorded as health.

## 5. Outputs

### `/p/<slug>.m3u`
```
#EXTM3U url-tvg="http://<host>/p/<slug>/epg.xml"
#EXTINF:-1 tvg-id="Arena.1.de" tvg-chno="2" tvg-logo="…" group-title="Sports",Arena 1
http://media-host:8080/ace/getstream?id=816b…
```
- Built per request from the DB, which is cheap and needs no gateway. The `ETag` is a hash of the rendered body, so route changes and resyncs invalidate it automatically.
- Only eligible channels are listed: the channel and its route are enabled (§4b). `tvg-chno` is the playlist position, so numbering does not shift when a channel is left out.
- Channels use the effective tvg-id (§4).
- `.m3u8` is also accepted as an alias.

### `/p/<slug>/epg.xml`

Built per request from the per-source caches, for the same eligible channels as the M3U (§4b). Output order is all `<channel>` elements first, then all `<programme>` elements, as the XMLTV DTD requires.

**Upstream channels**
- For each non-event channel in the playlist, take the matching `<channel>` and its programmes from the source caches.
- When the same id appears in several sources, the source with the highest priority wins, and only that source's programmes are used. No programme deduplication.

**Event channels** (`is_event`, on any route)
- Always emit a synthetic `<channel id="{effective_tvg_id}">` with the channel name as display-name. If an upstream source happens to carry the same id, the synthetic one replaces it.
- Programmes, relative to `now` truncated to the hour, covering now to +24h:
  - With `kickoff_at`: the logical blocks are "Pre-match" [−∞, kickoff), the title [kickoff, kickoff+2h), and the title marked "(ended)" [kickoff+2h, +∞).
  - Each block is clipped to the window [hour, hour+24h). Empty intersections are dropped.
  - So a kickoff 48h away gives a single Pre-match block, and an event that already ended gives a single "(ended)" block starting at the current hour.
  - Without kickoff: the title in a rolling block from now to +24h.
- These blocks are a guide placeholder, not the broadcast's actual state (checks report that, §9).

**Caching**
- The response is cached in memory, keyed by (playlist, output version, current hour).
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

You pick the route, then paste the input. `POST /api/v1/resolve {route_id, input}` validates it with that route only (§4b) and returns, without writing anything:
- the canonical source, identity, play URL, the gateway's hints and warnings;
- `duplicate_of`, an existing channel on the route with the same identity, and `duplicate_source` when that channel's stored source differs (for example a newer access token in the link). Editing that channel with the new input updates it ("Update source");
- suggestions and guide matches.

Suggestions come from the gateway's hints (`title`, `region`, `group`, `event`): `name` from the title (or a placeholder: host and last path segment for direct URLs, the identity for gateway routes), `group_title` from the group, the event flag. When there is a title, it is fuzzy-matched against `epg_channels.display_names` with normalized token similarity (strsim), with a bonus for a matching region suffix (`.de`, `.uk`, `.us`); the top 5 `{xmltv_id, display_name, icon, score}` are returned, and the best one's tvg-id and logo are suggested when it scores at least 0.6. Gateway warnings (such as a failed title lookup) never fail the resolve.

Site- or protocol-specific suggestion logic (cleaning engine names, region tags, a "Sports" guess) belongs to the gateway rule that produces the hints. The UI fills empty fields with the suggestions and offers the other matches and a guide search. In the CLI, the suggested name is always used unless `--name` is given; the other suggestions are applied with `--yes`, or after a confirmation prompt on a terminal. Explicit flags always win. Creating a channel validates the input again; a preview is never trusted.

## 8. Events and hooks

**Events**
- `playlist.changed {slugs}`
- `epg.updated {sources?}`: guide content may have changed. Emitted by source publishes, source config changes, and event-channel guide changes.
- `channel.unhealthy {channel_id, name, detail}`
- `channel.recovered {channel_id, name}`
- UI-only (SSE, not hook event types): `channel.checked {channel_id}`, `settings.changed {keys, revision}`, `route.changed {route_id, revision}`.

**Which mutations affect which outputs.** Every write goes through one service function. It computes the affected slugs *before* deleting anything, then invalidates caches and emits events.

| Mutation | Affected playlists | Emits |
|---|---|---|
| channel name / group / tvg_id / logo / source / route / enabled / is_event / kickoff | all containing the channel | `playlist.changed` (+ `epg.updated` if tvg_id / is_event / kickoff, or an event channel's name, changed) |
| channel delete | all that contained it (captured before delete) | `playlist.changed` |
| item add / remove / move / replace | that playlist | `playlist.changed` |
| playlist create / rename / delete | that slug | `playlist.changed` |
| route enable / disable | playlists with ≥1 channel of the route | `playlist.changed`, then `route.changed` |
| route base URL change, or a resync that changed channels | playlists with ≥1 channel of the route | `playlist.changed`, then `route.changed` |
| route create / rename / delete, a resync with no changes | — | `route.changed`, SSE only |
| settings change (guide refresh, check interval) | — | `settings.changed`, SSE only |
| EPG source publish | all | `epg.updated` |
| EPG source enable / disable / delete / priority change | all (EPG cache flushed) | `epg.updated` |
| EPG source URL change | — (current gen stays served until the new URL's first successful publish; fetches stay unconditional until then per §6, first one runs immediately) | `epg.updated` on that publish |
| health status transition | — | `channel.unhealthy` / `channel.recovered` (hooks and SSE) |
| check with no transition | — | `channel.checked`, SSE only |

**Dispatcher**
- One tokio task per hook, with a trailing-edge debounce of `debounce_secs` (default 30). A window never stays open longer than four debounce periods, so a steady stream of changes still gets delivered.
- The delivered body is the envelope `{events: [{type, at, payload}…], window_start, window_end}`, so mixed events keep their own payloads.
- Retries 3 times with backoff (2s, 10s, 30s). Each attempt is logged; the last 200 rows per hook are kept.
- A 2xx means *accepted*, not that the downstream is fresh.

**Hook kind** (webhooks only)
- `webhook`: `{method, url, headers{}}`. The body is always the envelope above. Body templates are deferred.
- Receivers are recipes, not kinds: Jellyfin is `POST {base}/ScheduledTasks/Running/{RefreshGuide task id}` with `Authorization: MediaBrowser Token="…"` (docs/deploy.md). lineup never discovers task ids or schedules extra refreshes; the user supplies the full URL.
- Every hook has a "test" action that sends a synthetic event.
- Secrets (header values) are write-only in the API and masked in responses.

## 9. Channel checks

A check asks the route's gateway whether a channel plays (`POST {base}/v1/check {rule, ref, force}`). Only gateway routes whose rule has the `check` capability can be checked; others get 409. How a check is done safely (for Acestream: a short engine session with its own pid, the acexy activity guard, stopping only its own session, one probe per engine at a time, refusing when the guard can't read status) is the gateway's job. Checks say whether a channel plays, not how well.

**Outcomes**: `ok`, `dead`, `unknown` (the gateway, its services or the network failed), `skipped` (not checked, e.g. `reason: busy` because someone is watching; `force` checks anyway). A check may also report a `content_state`: `live`, `ended_processing` (the broadcast ended and the recording is not ready; reported as `unknown`), `recording` (reported as `ok`). An ended broadcast is never a failure.

**Running**
- One channel at a time; runs never overlap.
- A gateway that fails (an HTTP-level error) gets `unknown` for the channel being checked and stops the run for that route: its remaining channels are reported as skipped, not recorded. A gateway outage never marks channels dead.
- Manual: `POST /checks {ids?: [], force}`. With ids the request waits and returns the results; without, every enabled channel of every enabled gateway route with checks is checked in the background (202).
- Schedule: 10 min after start, then every `check_interval_hours` (setting; default 0, which is **off**). A run in which channels were skipped, or a gateway failed, is retried after 30 min.

**Results**
- A result is dropped (not recorded) when the channel was deleted (even if its id was reused), its `source_revision` changed, or its route's revision changed since the check started.
- Results only write `channel_health`.
  - Every attempt updates the `last_attempt*` fields, `content_state`, `reason` and `detail`.
  - Only `ok` and `dead` results update `conclusive`.
- Events compare the new conclusive result with the previous `conclusive`:
  - `null|ok → dead` emits `channel.unhealthy`. The first result being dead also counts.
  - `dead → ok` emits `channel.recovered`.
  - No change emits `channel.checked` (SSE only).
- `unknown` and `skipped` never touch `conclusive`, so "dead, unknown, skipped, ok" emits exactly one recovery.
- The UI badge shows `conclusive`, with a hint when the latest attempt is inconclusive.
- Playlists are never edited.

## 10. REST API (`/api/v1`, JSON, OpenAPI via utoipa at `/api/openapi.json`)

```
GET    /channels?group=&q=&playlist=&route_id=&health=   POST /channels {name, route_id, input, …}   GET/PATCH/DELETE /channels/{id}
       PATCH: route_id and/or input revalidate the source (with the current source when only the route changes)
POST   /resolve {route_id, input}            -- preview: validated source, duplicate, suggestions; writes nothing
GET    /channels/{id}/info                   -- latest check + gateway observation (§4b); never starts a stream
POST   /checks {ids?: [id], force?: bool}    -- no ids = all checkable channels, in the background (202)
GET    /routes                               POST /routes {name, config, enabled}   GET/PATCH/DELETE /routes/{id}
       PATCH: name, enabled, base_url (a base URL change revalidates every channel first, all or nothing)
POST   /routes/test {config, input?}         -- test a draft config (and an input with it); lists a gateway's rules
GET    /routes/{id}/status                   -- the saved route's gateway, live: answers, its rule, channels pending a resync
POST   /routes/{id}/resync                   -- revalidate every channel; returns the report
POST   /refresh {slugs?: []}   -- emits playlist.changed for the given playlists (all if omitted)
GET    /playlists                            POST /playlists         GET/PATCH/DELETE /playlists/{slug}
PUT    /playlists/{slug}/items  {channel_ids: [id,…]}  (full membership + order replace; returns the playlist)
POST   /playlists/{slug}/items  {channel_id, position?}   DELETE /playlists/{slug}/items/{channel_id}  (both return the playlist)
POST   /playlists/{slug}/items/{channel_id}/move {position}
       Channel/playlist/route DELETE return 204. Import-by-URL bodies are capped at 2 MiB.
GET    /epg/sources  POST/PATCH/DELETE …     POST /epg/fetch         GET /epg/channels?q=  (search for tvg-id picker)
GET    /hooks  POST/PATCH/DELETE …           POST /hooks/{id}/test   GET /hooks/{id}/deliveries
GET/PATCH /settings                         -- {epg_refresh_hours, check_interval_hours, revision}
GET    /backup   -- consistent SQLite snapshot (VACUUM INTO); not in OpenAPI
POST   /import  {m3u_text | url, playlist_slug, route_id, dry_run}
       Every entry is validated with the route before anything is written; a gateway failure aborts the import
       with nothing written; entries the route does not accept are skipped and listed in skipped_lines.
       dry_run: strictly read-only; returns per-entry {name, source, identity, action: create|reuse|skip_in_playlist, diff vs existing}
       commit: one transaction (the route revision is checked again). Existing channel with the same identity on the route is
               reused as-is (metadata and source NOT overwritten; diff reported, including a different source);
               new ones created; missing members appended to the playlist in file order; playlist created if absent.
               Emits playlist.changed once. Returns the same per-entry report.
GET    /events (SSE)   -- live updates for the UI: channel/playlist/route changes, check results
```
- Errors use `{error: {code, message}}` with stable `code` strings, so the CLI can map them to exit codes: `not_found` (404), `conflict` (409), `invalid` (422), `gateway_unreachable` / `gateway_error` / `contract_mismatch` (502), `internal` (500).
- An M3U is not a backup (it lacks disabled channels, routes, events, hooks and guide sources); `GET /backup` is.

## 11. CLI

- The same binary: `lineup serve` runs the server, every other subcommand is a client.
- Server address: `--server` / `LINEUP_SERVER` / `~/.config/lineup/config.toml`.
- Built with clap derive. `clap_usage` produces a jdx usage spec (`lineup usage`), which drives completions and docs. `--json` on every command, plus `-q`.
- Exit codes: 0 ok, 1 generic, 2 usage/invalid input, 3 not found, 4 conflict/duplicate, 5 server unreachable, 6 gateway failed.

```
serve
add <input>... --route <route> [--name --group --tvg-id --logo --playlist <slug>] [--event --kickoff <time>] [--yes] [--dry-run]
resolve <input> --route <route>
ls [query] [--group --playlist --route --health dead] ; show <channel> ; info <channel>
edit <channel> [--name … --source <input> --route <route>] ; rm <channel> ; enable/disable <channel>
playlist ls|create <slug> [--name]|rm <slug>|show <slug>
playlist add <slug> <channel>... [--at N] ; playlist remove <slug> <channel>... ; playlist move <slug> <channel> <pos>
playlist url <slug>     # prints m3u + epg URLs
route ls | show <route> | add <name> (--direct | --gateway <url> --rule <rule>) [--disabled]
route edit <route> [--name --base-url --enable|--disable] | rm <route> | resync <route>
route test [<route> | --gateway <url> [--rule <rule>]] [--input <input>]     # lists the gateway's rules
check [<channel>...] [--force]
epg fetch ; epg sources ls|add|rm ; epg search <query>
hook ls|add|rm|test <id> ; hook deliveries <id>
refresh [<slug>...]
import <file|url> --playlist <slug> --route <route> [--dry-run]
settings get | set epg_refresh_hours <n> | set check_interval_hours <n>
backup <file>
usage                   # emit usage spec
```
- `<channel>` accepts an id, an exact name, or a channel's exact stored source or identity. `<route>` accepts an id or an exact name.

## 12. Web UI (Vue 3 + Vite + TS + @nuxt/ui v4 in Vue mode, vue-router, Pinia Colada; a hand-written manifest and service worker)

- **Channels**
  - Searchable table with group, health badge, route and identity, and playlist chips.
  - An "Add" slideover: pick the route (the last one used is preselected), paste the input, and see the validated source, play URL and suggestions with editable fields: tvg-id picker backed by `/epg/channels`, logo preview, playlist checkboxes. A duplicate whose stored source differs offers "Update it to this link".
  - Row actions: details, edit, check (gateway routes), copy play URL, open in VLC (`vlc-x-callback://x-callback-url/stream?url=…`) or Infuse (`infuse://x-callback-url/play?url=…`), delete. Channels of a disabled route show a "route off" badge.
  - Details (`/channels/:id`, a slideover; full width on phones): stored fields, what the gateway observes now (polled every 5 s while visible, one request at a time), latest check and Check now.
- **Routes**: one card per route with its config, the gateway's live status (rule, capabilities, channels pending a resync), the last resync report, and Resync / Enable / Edit / Delete. Adding a route loads the gateway's rules from its URL; any config can be tried with an input before saving.
- **Playlists**
  - A list, plus an editor with drag-to-reorder (position = chno) and add/remove channels.
  - Copy buttons for the m3u and epg URLs.
- **EPG**: sources with last fetch status, a "fetch now" button, and a channel search.
- **Hooks**: CRUD, test button, delivery log.
- **Settings**: the guide refresh schedule; the check schedule and Check all.
- **Import**: pick a playlist and a route, paste or upload an m3u, preview, confirm.
- **Live updates**: SSE invalidates queries.
- **PWA**: manifest, icons, `apple-mobile-web-app-capable`, safe-area insets, mobile-first layout using UDashboard* components that collapse to a bottom nav or drawer on phones.
  - Caveat: over plain HTTP on a LAN the service worker won't register on iOS. It still works as a home-screen web app, just with no offline support, which is acceptable.
- **Types**: TS types are generated from OpenAPI (`openapi-typescript`) and called through a thin `openapi-fetch` client.

## 13. Repo layout

```
Cargo.toml (workspace)
crates/
  server/            axum app, db, routes (validation), catalog (validate-then-write), checker, gateway client, epg, hooks, m3u
  cli/               clap client (depends on api-types)
  api-types/         shared serde DTOs + utoipa schemas
  gateway-contract/  wire types of the gateway contract v1, with JSON fixtures
  lineup/            the binary: dispatches `serve` → server, else → cli
web/                 Vue app (pnpm), builds to web/dist, embedded by server via rust-embed
docs/                spec.md, deploy.md, releases/
Dockerfile           multi-stage: node build web → rust build (musl) → alpine
.github/workflows/   ci.yml (fmt, clippy, test, web typecheck/build/test), release.yml (GHCR + binaries)
deploy/              example compose.yaml, restore.sh
```

The server's `test-support` feature adds a stub gateway (`lineup_server::testing::StubGateway`) used by the server and CLI tests.

## 14. Milestones

**Build order** (CI and Dockerfile):
1. `cargo run -p lineup-server --bin openapi > web/src/api/openapi.json`. The bin only builds the router metadata and needs no running server.
2. `pnpm gen:api && pnpm build`, which outputs `web/dist`.
3. `cargo build --release`. rust-embed reads `web/dist` relative to the server crate. `build.rs` writes a placeholder `web/dist/index.html` when it is missing, so `cargo check` works without node.
4. sqlx uses runtime queries, not `query!` macros, so there is no `.sqlx/` offline data and the build needs no database. Every query is covered by tests.

**M1–M4** (0.1–0.4, done): core and deployment, EPG and the add flow, hooks and health, polish (PWA, player links, usage/completions, release binaries).

**M5 Routes and gateways** (0.5): routes replace integrations; protocol knowledge (Acestream, VK) moves into gateways behind contract v1; generic checks, observation and resync; fresh schema lineage.

## 15. Open questions

- Does RefreshGuide re-download an M3U from a URL tuner, and how long is Jellyfin's XMLTV cache window (about 1h upstream)? Answered on a real install: a RefreshGuide webhook makes Jellyfin re-download both the M3U and the XMLTV URL within seconds. The cache window is unmeasured.
- Should a route carry its own check schedule, or is the global one enough?

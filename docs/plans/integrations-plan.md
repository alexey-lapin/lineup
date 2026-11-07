# Plan: integrations (replace hardcoded acexy/engine/VK settings) — lineup 0.4.0

Repo lineup, main at df9b56f. Owner decisions are fixed (grilled); review the
plan for gaps, risks and better shapes, not the decisions themselves.

## Owner decisions

- An *integration* = a type defined in code + its config + status + capabilities. One instance per
  type for now (model can hold a list later). Types: **Acestream** (engine required, acexy optional
  but recommended), **VK proxy** (kept as-is; may become a general streamlink type later), **Direct
  URL** (built in, nothing to configure). Every channel belongs to exactly one integration.
- New **Integrations** page (sidebar), one card each: reachable/unreachable, version, URL fields + Test,
  live numbers. Acestream card: engine version, acexy active-stream count (global `/ace/status`
  only, no per-channel polling), and the health-probe schedule (moved from Settings). Settings keeps
  app-wide items only (guide refresh).
- Acestream without acexy: playlist URLs fall back to `{engine}/ace/getstream?id=` (card hints that
  acexy is recommended for several viewers).
- Unconfigured integration (blank URL = off; no separate enable switch): its channels stay listed with
  a badge and are left out of playlists.
- **Channel detail view** (slide-over from the row on wide screens, full screen on phones), separate
  from the edit form: overview (playlists, guide match), integration section, health history, Edit.
  Acestream section: live peers / speeds / status, and acexy client count, read only while acexy
  reports the stream active (per-id `/ace/status?id=` gives `clients` + `stat_url`; reading
  stat_url is free); otherwise the last probe result plus a "Check now" button (starts a short engine
  session, like a probe). Opening the view never starts a session. Engine original name
  (`get_media_files`) fetched live on open, not stored. Polls every few seconds while open and active.
- Out of scope: stream format/ffprobe; health for VK/Direct (capability model must allow it later).
- 0.4.0, breaking, **no upgrade path**: squash migrations 0001-0005 into one new 0001 schema; an old
  database makes the server refuse to start with a clear message. Owner re-creates: fresh DB,
  re-import playlist, re-add guide sources and hook. Settings API + CLI change shape, no shims.

## Facts (checked)

- Today: `Settings {acexy_base, engine_base, vk_base, epg_refresh_hours, probe_interval_hours,
  revision}` in a key/value `settings` table (JSON values); `PATCH /settings`, `POST /settings/test`
  (target acexy|engine|vk; engine test returns version). Web: SettingsPage + components/settings.
- `Source` enum `Acestream{id} | Vk{owner_id, video_id, list} | Raw{url}`; channels table
  `source_kind CHECK IN ('acestream','vk','raw')`, `source` JSON, `UNIQUE(source_kind, source_key)`.
- `source::stream_url` builds `{acexy}/ace/getstream?id=` / `{vk}/vk/{o}_{v}[?list=]` / raw verbatim.
- Spec §: a base change emits `playlist.changed` for playlists with ≥1 channel of that kind.
- probe.rs: acexy guard (per-id status, global fallback, fail closed), engine session with
  `format=json&pid=lineup-probe-<uuid>`, stat_url rebased onto engine base, `peers`/`status`.
- Live: engine 3.2.11 `/webui/api/service?method=get_version`; acexy 0.2.2 (latest) has no version
  endpoint; global status `{"streams":N}`; per-id `AcexyStatus{clients, streams, stream_id, stat_url}`
  (acexy lib/acexy/acexy.go), 404 when inactive.

## Proposed design

Server
- `integrations` table: `kind TEXT PRIMARY KEY CHECK IN ('acestream','vk','direct')`, `config` JSON,
  `updated_at`; rows seeded by the schema (blank config). Or keep it in the settings k/v table under
  `integration.<kind>` — reviewer's call; leaning on a table.
- api-types: `IntegrationKind`, `Integration {kind, config: IntegrationConfig, configured: bool,
  capabilities: [...], revision}`, tagged `IntegrationConfig::Acestream{engine_url, acexy_url:
  Option, probe_interval_hours} | Vk{url} | Direct{}`; `IntegrationStatus {reachable, version,
  detail, extra}` (acestream extra: acexy reachable, active_streams).
- Code: `integrations` module, one sub-module per kind behind a small trait or an enum match:
  `stream_url(source, config) -> Option<String>` (None = unconfigured), `test/status`, `channel_info`,
  `health` (acestream only). Leaning on enum match over a trait object (three fixed kinds).
- `SourceKind` gains/renames `raw -> direct`? (wire break is allowed; keep `raw` if cheaper).
- Routes: `GET /integrations`, `GET/PATCH /integrations/{kind}`, `POST /integrations/{kind}/test`
  (optional unsaved config), `GET /integrations/{kind}/status` (live, cheap),
  `GET /channels/{id}/info` (live: integration section + last health), `POST /channels/{id}/check`
  (Check now = single probe, guarded like scheduled probes). Remove settings URL fields + settings
  test; `Settings` keeps `epg_refresh_hours` (+ revision).
- Playlist render skips channels whose integration is unconfigured. Config change -> event
  `integration.changed` + `playlist.changed` for affected playlists (same rule as today's base change).
- Probe scheduler reads the interval from the Acestream config; engine-only operation when acexy is
  blank (guard: no acexy => cannot know viewers => ? see Q3).
- Old DB detection: before migrating, if `_sqlx_migrations` has any version > 1 or the 0001 checksum
  differs, exit with "database from lineup <= 0.3; start with an empty data dir (see release notes)".

CLI
- `lineup integration ls | show <kind> | set <kind> [--engine-url --acexy-url --url
  --probe-interval] | test <kind>`; `lineup channel show <id>` (info) and `lineup channel check <id>`;
  `lineup settings` keeps guide refresh only.

Web
- IntegrationsPage (cards, status polled while visible), Settings trimmed, ChannelDetail
  (slide-over/route `/channels/:id`), row click opens it, badge for unconfigured integration,
  ChannelForm unchanged except source hints per integration.

Docs/tests
- spec.md, deploy.md, README, release notes v0.4.0 (fresh start steps), openapi regenerated.
- Tests: schema squash equals old 0005 end state (except removed bits); old-DB refusal; stream_url per
  config incl. engine fallback; playlist skips unconfigured; integration PATCH emits events; info
  endpoint with fake engine/acexy (active vs inactive, never starts a session on GET); check endpoint
  guarded; CLI integration commands; web tests where present.

## Questions for the reviewer

1. Table vs settings k/v for integration config; enum match vs trait.
2. Rename `raw` -> `direct` on the wire/DB now (free with the squash) or keep `raw`?
3. Probes/Check now without acexy: there is no way to know whether someone is watching through the
   engine directly. Refuse Check now and skip scheduled probes when acexy is blank, or allow them
   (risk: a probe session on a watched id)? Engine sessions with distinct pids don't stop each other's
   playback as far as we know — is that a safe assumption?
4. Info endpoint while active: read acexy per-id `stat_url` (rebased onto the engine) — any caveat
   (stat_url host/loopback, acexy without per-id status)?
5. Is squash + refuse-old-DB the cleanest "no upgrade" implementation with sqlx (checksums, the
   `_sqlx_migrations` table)?
6. Anything missing: events, hooks payloads mentioning settings, SSE live updates, CLI completions/usage
   spec, docs.

## Resolutions after REPORT #42 (all of Codex's answers adopted; owner decisions below)

Blocking 1 (old-DB detection): identify the lineage by the applied version-1 checksum against the
embedded new 0001; a mismatch (or an untracked but populated DB) refuses to start with the
"<= 0.3, start with an empty data dir" message; otherwise normal SQLx validation (later migrations,
newer-than-binary, dirty) with its own diagnostics. Never edit the ledger or ignore checksums. Keep
a frozen legacy fixture (0.3 DB) for the refusal test; test fresh, reopen, legacy 0001-only, legacy
0005, and new lineage + an extra migration.

Blocking 2 (health history): OWNER SCOPE CHANGE: no history for now. The detail view shows the
current summary only (last attempt + last conclusive, as channel_health holds today). No attempt log.

Q1: dedicated `integrations` table (kind PK, config JSON, revision INTEGER incremented
transactionally on effective change, updated_at), enum match. Seeded complete typed configs
(Acestream engine_url "", acexy_url null, probe_interval_hours 0; Vk url ""; Direct {}). Route kind
must equal the config tag. PATCH: omitted = keep, explicit null = clear acexy_url; CLI `--acexy-url ""`
clears. Blank required URL = off; non-blank validated. Keep section.ts save-order/revision guards,
per-card drafts, unsaved-config Test.
Q2: rename raw -> direct everywhere now (Source tag, SourceKind, CHECK, CLI --kind, import/resolve,
schemas, fixtures, docs); identity rules unchanged. Integration membership derived from SourceKind.
Q3: OWNER CONFIRMED: channel checks (Check now + scheduled probes) unavailable without acexy, and
whenever the guard cannot establish inactivity; the card explains why. Connection tests (engine,
acexy) always work. Keep `force` only as an explicit CLI override if it exists today. Document that
the guard only sees acexy clients, not players connected to the engine directly.
Q4: passive stats only via acexy per-id stat_url; never getstream/command/stop on acexy's session;
rebase onto the configured engine origin preserving any base path (fix probe::rebase accordingly);
fetch only the stat endpoint, no redirects; per-id unsupported -> "live details unavailable"; stream
gone between calls -> inactive; bounded time/body; no URLs in logs/errors; partial results.
Q5: as Blocking 1; release notes keep the old data dir/backup aside and say a 0.3 backup needs the
0.3 binary.
Q6 adopted as contracts:
- configured (stable config property) != reachable (transient). Unreachable-but-configured stays in
  output. Acestream configured = engine_url set. Unknown counts shown as unknown, not 0.
- Channel.stream_url and ResolveResponse.stream_url nullable + `configured` flag; CLI/UI copy/open
  actions handle null; unconfigured channels can still be created/imported.
- One eligibility rule for M3U and XMLTV (enabled AND integration configured), consistent snapshot,
  positions preserved, guide cache invalidated on eligibility change; tests for disable/re-enable.
- Mutation table: playback URL or eligibility change -> playlist.changed for affected slugs + cache
  invalidation; schedule-only -> integration.changed only (UI-only like settings.changed; payload
  kind + revision, no URLs). SSE ready/resync and query invalidation cover integrations, status,
  channels, playlists, open details. Hook EVENT_TYPES explicit.
- Checks snapshot config, keep task serialization/cleanup, stop only their own session, drop late
  results after source/config change, keep skipped/unknown vs conclusive semantics; passive stats never
  write health.
- UI polling only while visible, no overlap, cancel stale, stop on close/hide; engine name once per
  open; errors shown per section; Test uses the draft, status uses saved config.
- CLI: keep today's flat channel commands shape decided in implementation (show/probe/force as now,
  plus info output); usage/completions, OpenAPI, source filters, docs; test the fresh owner workflow.

# Review: lineup 0.4.0 integrations, implementation (steps 1-3)

Repo lineup, branch `integrations` (3 commits on main df9b56f):
- c683c1b server, cli: integrations replace the service URL settings
- b1cafd7 web: Integrations page; Settings keeps the guide refresh
- 2808200 web: channel detail view

Agreed plan: docs/plans/integrations-plan.md (the
"Resolutions after REPORT #42" section is authoritative). Docs (spec.md, deploy.md, README, release
notes) are NOT done yet: that is the next step; ignore their staleness, but list what must change.

Review the diff `git diff main..integrations` against the plan: correctness bugs, gaps vs the agreed
contracts, risky choices. Key places:
- crates/server/migrations/0001_init.sql (squashed schema, seeded integrations rows)
- crates/server/src/store.rs check_lineage + Store::open (lineage checked on a plain connection
  before the WAL switch so a 0.3 file stays byte-identical); tests/lineage.rs with frozen 0.3
  migrations in tests/fixtures/migrations-0.3
- crates/server/src/integrations.rs (Configs, playback_base, apply), store/integrations.rs
  (patch + mutation table: playlist.changed only when playback_base changes; integration.changed always)
- eligibility: store::eligible used by render_m3u and epg/output.rs
- probe.rs (ensure_available: checks need engine + acexy, also with force; rebase keeps base path;
  ProbeIdentity.engine_url drops results measured through a replaced engine)
- live.rs (channel info: per-id acexy status, stat_url rebased + restricted to /ace/stat/, no
  redirects, bounded body, 404/unknown session -> inactive, 4xx -> no per-stream status)
- connection.rs (component tests and status), http.rs routes, CLI (integration ls|show|set|test, info)
- web: components/settings/section.ts (generic useSection), pages/IntegrationsPage.vue,
  components/ChannelDetail.vue, composables.ts usePolling + SSE, ChannelsPage.vue

Decisions made in implementation (challenge if wrong):
- No separate /channels/{id}/check route: Check now uses POST /probe {ids:[id]} (409 when unavailable).
- New `lineup info <channel>` instead of changing `lineup show` JSON.
- Engine name: GET /channels/{id}/info?name=true once per open; polling uses name=false every 5 s.
- PATCH /settings ignores unknown fields (old acexy_base etc. are silently dropped).

Verification done: cargo test --workspace, clippy clean, vitest 24 pass, vue-tsc clean, manual run
against a fake engine/acexy (UI screenshots fine), a real 0.3 DB copy refused untouched.

Write findings to /tmp/lineup-integrations-impl-codex.md; end with BLOCKING: <list> or AGREE.
Read-only review: do not change files.

# Review brief: M2 checkpoint 1, EPG ingestion

Repo: lineup, commit 39b3dcf (`git show 39b3dcf`). Do not edit repo files.
Spec: docs/spec.md sections 4 (epg tables), 6 (EPG ingestion), 8 (events table rows for EPG sources), 10 (API).

## Scope of this checkpoint
- migrations/0002_epg.sql: epg_sources, epg_channels.
- crates/server/src/epg/xmltv.rs: building a generation from the raw download (gzip detection, streaming quick-xml parse, programmes re-emitted event by event, grouped by channel and sorted by start, per-channel byte-range index, channels.json, meta.json, COMPLETE marker with fsyncs). Programmes that ended more than 6h ago are dropped. A document without closing </tv> is rejected.
- crates/server/src/epg/mod.rs: Epg service. Fetch sequence (validators only when the current generation is complete and meta.url equals the configured URL; 304 handling; new generation dir = max+1; publish; keep only current and previous), single global fetch lock, startup recovery, background fetch on create/URL change, directory removal on delete, boot-relative wall-clock scheduler (3 min, then every epg_refresh_hours, 0 = off).
- crates/server/src/store/epg.rs: source CRUD, publish/republish transactions, Event::EpgUpdated emission rules (publish of an enabled source; enable/disable/priority change of a source with data; delete of an enabled source with data; URL change alone emits nothing).
- crates/server/src/suggest.rs: fuzzy ranking used by GET /epg/channels?q= (and later by resolve suggestions in CP3).
- http.rs: AppState is now `App {store, epg, scratch}` (Deref to Store); new endpoints. POST /epg/fetch is synchronous.
- CLI: epg fetch|sources ls/add/edit/rm|search, settings epg_refresh_hours; post_slow (1h timeout) for fetch.
- Tests: crates/server/tests/epg.rs (local upstream server: 304, generations, broken download, URL change, disabled, recovery), xmltv and suggest unit tests, CLI test.

Not in this checkpoint (coming next): /p/<slug>/epg.xml output, VK placeholders and the response cache (CP2); resolve suggestions, the add flow and the web UI for EPG (CP3).

## Please check
1. Correctness against spec section 6, especially crash safety and recovery, validator rules, generation numbering and cleanup, and races between fetch, delete and URL change.
2. XMLTV parsing robustness (entities, CDATA, self-closing elements, huge files and memory use, malformed input) and whether the re-emitted fragments are faithful.
3. Event emission rules vs the spec section 8 table.
4. API shape and errors, CLI behaviour.
5. Anything else blocking.

Write findings to /private/tmp/lineup-m2-cp1-codex.md, ending with AGREE or BLOCKING plus a numbered list.

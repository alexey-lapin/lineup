# Review brief: M2 fixes, M2 CP3, M3 SSE and hooks

Repo lineup. Do not edit repo files. Commits, oldest first:

1. 950f2d3 (M2 CP3, add flow): resolve.rs (engine name lookup via get_media_files, name cleanup, region hint, sports heuristic, top-5 guide matches, suggestion when score >= 0.6, warnings never fail the resolve); ResolveResponse gained engine_name, suggestion, matches, warnings; CLI add applies suggestions beyond the name only with --yes or after a TTY prompt; web: EPG page, EpgSearch component, add-form suggestions and guide picker, guide URL buttons, epg_refresh_hours setting. Spec section 7.
2. 696adc5 (M3, SSE): GET /api/v1/events (ready, playlist.changed, epg.updated, resync on lag; streams end on shutdown via a watch channel); web useLiveUpdates.
3. edf0fbd (M3, hooks): migration 0003, store/hooks.rs (CRUD, validation, write-only secrets with SECRET_MASK round-trip, delivery log trimmed to 200 per hook), hooks.rs dispatcher (supervisor plus one worker per enabled hook, trailing debounce capped at 4 periods, retries 2s/10s/30s, Jellyfin RefreshGuide via task lookup with cached id, single follow-up moved later by each delivery, test action), HTTP /hooks, CLI lineup hook, web Hooks page. Spec section 8.
4. 1554eb5: fixes for REPORT #18 findings 1-3 plus note 5. Note 4 (element limit applies after quick-xml has buffered a whole event) is left as a known limitation: bounding it needs a custom reader; the decompressed document cap still bounds the total.

Please check correctness against the spec, races (dispatcher reload vs events, follow-up scheduling, SSE shutdown), secret handling (never logged or returned), API/CLI/UI consistency, and anything blocking.

Write findings to /private/tmp/lineup-m2m3-codex.md, ending with AGREE or BLOCKING plus a numbered list.

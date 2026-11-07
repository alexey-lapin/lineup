# Review: logging (commit 47a7106)

Repo: lineup, main. Review with `git show 47a7106`.

Goal from the owner: "better log than not". Before this, lineup logged only failures; a successful
guide reload, hook delivery or playlist change left no trace in `docker logs`.

What was added (all at the default `LINEUP_LOG=info` unless noted):

- `epg/mod.rs`: per-source fetch start, then updated (channels, programmes, seconds) / not modified /
  discarded (source changed mid-fetch); scheduled run start and summary counts.
- `hooks.rs`: successful delivery (event types or trigger, HTTP code, attempt); failed attempts now name
  what was being delivered; a "giving up" line after the last retry.
- `probe.rs`: run start (count, forced), each channel's outcome and detail JSON, run summary.
- `lib.rs`: a task subscribed to the event bus logs every event (`event <kind> <payload>`);
  `channel.probed` only at debug. Server now uses `into_make_service_with_connect_info::<SocketAddr>()`.
- `http.rs`: `log_request` middleware on the whole router: client IP (from ConnectInfo when present),
  method, path, status, ms. Info for non-GET/HEAD, any `/p/...` download, and 5xx; debug otherwise.
- `docs/deploy.md`: a Logs section.

Please check:

1. Correctness: anything that changes behaviour (middleware placement, connect-info, SSE `/events`
   long-lived responses, the retry loop restructure in `deliver`, the event logger lagging/closing).
2. Secrets or sensitive data in logs: hook URLs/headers/API keys, query strings, payloads.
3. Noise: anything that will spam at info (UI polling, SSE, probes, scheduler ticks), or important
   things still missing.
4. Tests worth adding.

Reply with REPORT #31: BLOCKING / NON-BLOCKING findings, or AGREE.

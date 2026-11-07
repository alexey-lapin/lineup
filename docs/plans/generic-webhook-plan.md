# Plan: generic webhooks only (drop the Jellyfin hook kind)

Repo: lineup, main. Target release 0.3.0 (breaking, allowed: single install,
"move fast"). The owner's instance has one Jellyfin hook (created without --task-id) that the release
must carry over.

## Goal (owner)

lineup should be client-agnostic. Replace the Jellyfin hook with the generic webhook. Drop the
65-minute follow-up resend ("weird and hardcode-ish"). Jellyfin keeps working as a webhook recipe:
`POST {jellyfin}/ScheduledTasks/Running/{RefreshGuide task id}` with header
`Authorization: MediaBrowser Token="<key>"`; Jellyfin ignores the JSON body.

## Changes

API / types (crates/api-types):
- `HookConfig` keeps only `Webhook { method, url, headers }` (still `kind`-tagged, so the wire shape of
  webhooks is unchanged and another kind can be added later).
- `DeliveryTrigger` loses `Followup` (Events, Test).
- HookCreate default events: every event type (as webhooks today).

Server:
- hooks.rs: remove follow-up scheduling, task-id cache, `find_refresh_task`, `jellyfin_auth`, the
  `minute` timing unit. Worker = debounce + deliver with retries, as for webhooks today.
- store/hooks.rs: drop Jellyfin validation/masking branches.
- Migration 0005: `DELETE FROM hook_deliveries WHERE trigger = 'followup'` and rebuild
  `hook_deliveries` with `trigger IN ('events','test')`; rebuild `hooks` with `kind IN ('webhook')`
  AFTER legacy rows are converted (see below), or leave the hooks CHECK as is and only stop writing
  'jellyfin' (simpler; preference?).
- Legacy conversion of existing Jellyfin hooks (config JSON `kind: "jellyfin"`), run once at startup
  before the hook dispatcher loads hooks, on raw JSON (the typed enum no longer has the variant):
  - URL `{base_url}/ScheduledTasks/Running/{task_id}`, method POST, headers
    `{Authorization: MediaBrowser Token="<api_key>"}`; name, events, debounce, enabled kept;
    followup_minutes dropped.
  - task_id: the stored one if set; otherwise look it up once (`GET {base}/ScheduledTasks`, key
    `RefreshGuide`, same auth, 10 s timeout). The id is not derivable offline (checked: not an MD5 of
    the task type name).
  - Lookup fails (Jellyfin down at boot, wrong key): convert anyway with URL
    `{base}/ScheduledTasks/Running/REFRESH_GUIDE_TASK_ID`, `enabled = false`, log a WARN naming the
    hook and the docs recipe. Never leave a jellyfin row behind (so typed reads never fail).
  - Idempotent (only rows whose JSON kind is jellyfin), one transaction per hook, logged at INFO.
  Alternative considered: a pure SQL migration (json_object) converting rows with a task_id and
  disabling the rest; rejected because the owner's hook has no task_id and would come back disabled.
- Deliveries rows with trigger 'followup' are removed by the migration (history only).

CLI: remove `hook add jellyfin`; `hook add webhook <name> <url> [--method] [--header Name=value]...`
stays; `hook test` help no longer mentions Jellyfin; output drops the jellyfin/follow-up branches.

Web (HooksPage.vue): no kind selector; the form is name, method, URL, headers (values masked on edit,
as now), events, debounce. List shows `METHOD url`. Intro text: hooks call a URL when playlists, guide
data or channel health change; e.g. a media server's refresh endpoint.

Docs (deploy.md "Tell your players about changes"): generic webhook section plus a Jellyfin recipe:
create an API key, find the task id
(`curl -s -H 'Authorization: MediaBrowser Token="KEY"' http://jellyfin:8096/ScheduledTasks | jq -r
'.[] | select(.Key=="RefreshGuide") | .Id'`), then
`lineup hook add webhook Jellyfin http://jellyfin:8096/ScheduledTasks/Running/<id> --header
'Authorization=MediaBrowser Token="KEY"' --events playlist.changed,epg.updated`. Note Jellyfin caches
the downloaded XMLTV for about an hour, so guide changes may lag; no automatic resend. spec.md: hooks
section updated (history notes kept). README hook line.
Release notes for 0.3.0 call out the breaking change and the automatic conversion.

Tests:
- conversion: with task_id; without task_id against a fake Jellyfin (/ScheduledTasks); lookup failure
  -> disabled with placeholder URL; non-legacy rows untouched; runs twice harmlessly; converted hook
  secrets masked in API output and delivered with the Authorization header.
- existing webhook tests keep passing; remove follow-up tests.
- web tests (if HooksPage has any) and vue-tsc; openapi/schema regenerated.

## Questions

1. Startup conversion with a network lookup vs SQL-only: acceptable? Lookup at boot may race Jellyfin
   starting (same host, different container); fallback is disabled + WARN.
2. Rebuild the hooks table CHECK, or leave it?
3. Should the webhook gain a "no body" option for receivers that reject a JSON body? (Jellyfin accepts
   it; leaning no until needed.)
4. Anything else Jellyfin-specific that the conversion would lose (e.g. the 404 -> re-lookup of a stale
   task id)?

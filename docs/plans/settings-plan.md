# Plan: Settings page refactor

Repo: lineup at main (HEAD 4084744). Today
`web/src/pages/SettingsPage.vue` is one form with three plain sections (Stream sources with all three
base URLs, Guide, Health checks) and one Save button for everything.

## Goal (from the owner)

Sections instead of one flat form:

- **Sources**, split into **acexy**, **Acestream engine** and **VK proxy**.
- **Guide**.

## Proposed layout

One page, a column of cards (`UPageCard`-style, or `UCard` with a header). Each card owns its fields,
has its own **Save** (enabled only when that card has unsaved changes) and **Reset**, and shows a short
line about what the setting affects.

1. **Sources** (a group heading; wording in "Phone" below: acexy and the VK proxy must be reachable
   by Jellyfin and players, the engine only by the lineup server)
   - **acexy** card: `acexy_base`. Help: Acestream channels play from `{base}/ace/getstream?id=…`.
     Shows how many acestream channels use it ("used by 12 channels").
     **Test** button (see backend below).
   - **Acestream engine** card: `engine_base`. Help: used to look up channel names when adding, and
     for health checks; players never connect to it. Contains the health-check schedule
     (`probe_interval_hours`), since probes run through the engine, plus a "Check all now" button
     (the existing `POST /probe` with no ids). **Test** button.
   - **VK proxy** card: `vk_base`. Help: VK channels play from `{base}/vk/{owner}_{video}[?list=…]`.
     Channel count. **Test** button.
2. **Guide** card: `epg_refresh_hours`, a summary of guide sources (count, how many failed last
   time, link to the EPG page), and "Fetch all now" (existing `POST /epg/fetch`).
3. Footer: version (`/status`), links to the API docs (`/api/openapi.json`).

Saving a card PATCHes only that card's fields, so unsaved edits elsewhere are neither saved nor lost.

Per-card state (a `useSettingsSection(keys)` composable): the latest confirmed server values for its
keys, the draft, dirty (draft vs confirmed, on normalized values), pending and error. Rules:
- Cards are disabled until settings load; load failures show an error with Retry.
- While a card's save is pending its inputs are disabled; other cards stay editable.
- A save updates only that card's confirmed values from the response, never other cards' drafts.
- A refetch updates confirmed values; clean cards adopt them; a dirty card keeps its draft and shows
  "changed on the server" only if one of its own keys changed (no false conflicts). Reset adopts the
  latest server values.
- Errors: the API call is made directly (not via `useAction`, which swallows the error) so a 422
  message is shown inline under the card; other errors also toast.
- Leaving with unsaved changes asks (vue-router `onBeforeRouteLeave`; `beforeunload` registered only
  while dirty). Documented limitation: an iPhone home-screen app that is backgrounded and killed
  loses drafts without warning; drafts are not persisted and nothing is saved implicitly.

Actions use saved settings: "Check all now" (probe, `force: false`, activity guard applies) and
"Fetch all now" are disabled while their card is dirty, with a hint to save first. Test uses the
typed URL and shows which URL it tested; a result is dropped if the input changed meanwhile.

Phone: single column, full-width inputs, `type="url"` with capitalization/spellcheck off, buttons
wrap and are at least 44 px tall with spacing between Reset and Save, long URLs and results wrap,
results are announced inline (aria-live) as well as toasted. Existing safe-area handling is kept.
The Sources heading explains that acexy and the VK proxy must be reachable by Jellyfin and players,
while the engine only needs to be reachable by the lineup server (acexy too, for the activity guard).

Base URL inputs get light client-side validation (must parse as http(s) URL) mirroring the server's
`normalize_base`; the server stays authoritative and its 422 message is shown under the field.

## Backend addition: connection test

Spec §12 already promised a "test" ping. Add `POST /api/v1/settings/test {target, base?}`:

- `target`: `acexy` | `engine` | `vk`. `base`: optional, to test an unsaved value; defaults to the
  stored one. Validated with the same `normalize_base`.
- Probes, 5 s timeout, never side effects:
  - acexy: `GET {base}/ace/status` → `ok` if HTTP 2xx and JSON; report `streams` if present. Labelled
    "acexy answered", not "playback works".
  - engine: `GET {base}/webui/api/service?method=get_version` (the documented "is the engine running"
    call). `ok` only for 2xx with `error: null` and a `result.version`; report version and platform.
    A non-null `error` is `ok: false` with that error. Any other answer (non-JSON, other shape) is
    `ok: false`, "HTTP <status> but not an Acestream engine response"; no success fallback.
  - vk: `GET {base}/` → `ok` if any HTTP response, labelled "an HTTP service answered (status N)";
    the proxy has no known status endpoint.
  - Paths are appended to the normalized base, keeping any base path prefix. Bodies are read with a
    small cap (64 KiB). 5 s deadline. Nothing that starts a stream.
  - The result says which URL was tested, and that the test ran from the lineup server (it cannot
    prove Jellyfin or a phone can reach the address).
- Response `{ok: bool, url, detail: string, http_status?: u16, version?: string, elapsed_ms}`;
  transport errors are `ok: false` with the message, not an API error. An invalid target/base stays a
  422 API error.
- Note: this lets a LAN user make the server issue GETs to arbitrary hosts (SSRF-ish). lineup is
  LAN-only with no auth by design (spec non-goals) and import-by-URL and EPG sources already do this,
  so accepted, documented.
- CLI: `lineup settings test acexy|engine|vk [--base URL]`. Exit 1 for a completed negative test
  (the result is still printed, and with `--json` still emitted); usage/network errors keep their
  existing codes.

## Settings change notifications

Today `patch_settings` only emits `playlist.changed` for playlists affected by a proxy change, and
the UI never refetches settings on events. Add a UI-only `Event::SettingsChanged { keys }` emitted
after a committed effective change (keys that actually changed), sent on SSE as `settings.changed`.
It does not bump the output version (outputs are covered by `playlist.changed`) and is not in
`EVENT_TYPES`, so hooks cannot subscribe to it and Jellyfin is never refreshed by it. The web
`useLiveUpdates` invalidates `['settings']` on it. Test: a connected SSE client sees
`settings.changed` for engine and schedule changes, and nothing for a no-op patch.

Channel counts per card come from the existing unfiltered `useChannels()` query; the guide summary
from the `['epg-sources']` query. Both handle loading/failure (the page may be opened directly).

## Files

- `crates/api-types`: `SettingsTestRequest`, `SettingsTestTarget`, `SettingsTestResult`.
- `crates/server/src/http.rs` + new `crates/server/src/settings_test.rs` (the probes); tests with a
  local fake server (acexy status JSON, engine version JSON, plain 404 for vk root → reachable).
- `crates/cli`: `settings test`.
- `web/src/pages/SettingsPage.vue` split into `components/settings/SettingsCard.vue` (title,
  description, dirty badge, Save/Reset/Test slots) and the page composing four cards; a small
  `useSettingsSection(keys)` composable owning per-card state, dirty tracking and save.
- `crates/server/src/store.rs` (emit `SettingsChanged`), SSE test; `web/src/composables.ts` listener.
- Vitest with deferred responses: saving one card while editing another; editing/resetting during a
  pending save; a refetch changing another card's keys keeps a dirty draft without a conflict notice,
  one changing its own keys shows it; a stale Test result is dropped; inline 422; leave guard.
- Server tests for the connection test: acexy JSON, the documented engine envelope, a non-null engine
  error, malformed JSON, timeout, VK root 404, invalid base 422.
- OpenAPI/schema regenerated; docs/deploy.md settings table mentions the Test buttons.

## Out of scope

No change to settings storage or keys; no new settings. Hooks stay on their own page.

## Questions for review

1. Is per-card Save better than one sticky save bar for this small page?
2. Is the connection-test design (targets, endpoints, SSRF acceptance) sound?
3. Anything missing for phone layout (the page is used from an iPhone home screen)?

## Changes after review (Codex REPORT #24)

1. Engine test uses the documented `/webui/api/service?method=get_version` with `result.version`;
   explicit errors and unrecognised answers are failures, no success fallback.
2. Added the `settings.changed` UI event and client invalidation; previously promised SSE behaviour
   had no delivery path.
Also adopted: per-card state rules, saved-vs-draft actions, inline 422, test-result races, labels
for what a test proves, phone details, the iPhone unload limitation.

Codex REPORT #25: AGREE.

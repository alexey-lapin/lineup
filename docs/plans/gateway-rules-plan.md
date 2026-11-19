# Plan: config-driven rules for lineup-gateway

Repo lineup, main at ebb2c22. lineup itself is protocol-agnostic since #13; this plan does the same
for lineup-gateway's code. The gateway's job is still protocol work, but every site- or
service-specific fact moves from Rust into configuration, and the code keeps a few generic
mechanisms. Owner decisions are fixed; review the plan for gaps, risks and better shapes, not the
decisions themselves.

Revision 3: folds in two design reviews (state ownership, behaviour preservation, validation,
opaque refs, token round trips).

## Owner decisions

- **Config-driven rule instances.** A rule is a named block in the config with a `kind`. Any number
  of instances per kind, names chosen by the operator. A `Rule` trait and a name → rule registry
  replace the hard-wired `acestream`/`vk` fields and `match` arms.
- **Fully config-driven page handling.** No site code: VK's link forms, canonical ref and identity
  become match patterns and templates in the example config.
- **Extractors are config.** yt-dlp and streamlink are command + args + output format entries, not
  code paths.
- **Acestream: play without acexy.** The play URL is a template, so players can go to acexy, the
  engine directly, or any other proxy. Name clean-up lists and timings stay in code for now.
- **No embedded scripting (Lua) for now.** The escape hatch is an extractor that runs any script
  printing a documented JSON descriptor. A `kind = "lua"` backend can be added later without
  redesign.
- **Breaking config change is fine.** New format only, version bump, migration note. The example
  config keeps the rule names `acestream` and `vk`, so existing lineup routes keep working once the
  operator rewrites the config.

Decided while folding in the review (owner may overrule):

- **Acestream checks are opt-in** (`checks = true`, requires `acexy_control`). The guard only sees
  viewers who play through acexy; with a play template that bypasses acexy, the operator must
  leave checks off. Load fails for `checks = true` without `acexy_control`.
- **Acestream `id`/`content_id`:** an invalid `id=` now falls back to `content_id=` (today it does
  not). Deliberate loosening, documented.

## Facts (checked)

- `config.rs`: fixed `acestream: Option<AcestreamConfig>`, `vk: Option<VkConfig>` (empty), `tools`
  (`yt_dlp`, `streamlink` paths), `egress.allow`, `limits`. `deny_unknown_fields` everywhere.
- `lib.rs`: `Gateway { acestream, vk: Option<Relay>, _proxy }`. `info`, `matching`, `observe`,
  `check` each match on `"acestream"`/`"vk"`. `play` accepts only `vk.m3u8`; `resource`
  (`/play/s/{sid}/{file}`) always uses the VK relay. Egress proxy, extractor and relay start only
  when `[vk]` is present. `tool_version` (lib.rs:271) runs tools with the inherited environment
  and unbounded output.
- `rules/vk.rs`: hosts `vk.com`, `vk.ru`, `vkvideo.ru` (+ subdomains). Precedence: the gateway's own
  `…/play/vk.m3u8?ref=` (any host, recursive), then non-VK hosts → legacy `…/vk/<o>_<v>`, then
  `?z=video<o>_<v>…` (anything after the video digits ignored), then `/video_ext.php?oid=&id=`,
  then `video`/`live-` markers anywhere in the path. Owner/video parse as `i64` (so `-001` → `-1`,
  `02` → `2`; video > 0; a numeric video prefix is enough). `list` kept only if
  `[A-Za-z0-9_-]{1,64}`; `video_ext.php` falls back to `hash`, which is not filtered and is
  percent-encoded in the canonical ref. Identity `vk:<o>_<v>`, ref
  `https://vk.com/video<o>_<v>[?list=]`, play `play/vk.m3u8?ref=<ref>`, group hint `VK`.
  `MATCH_REVISION`/`PLAY_REVISION` are hand-bumped constants.
- `rules/acestream.rs`: `parse_id` accepts `acestream://<id>` (case-insensitive scheme, all
  trailing slashes trimmed), a bare 40-hex id (no trailing slash), or an http(s) URL with `/ace/` in
  the path and `id=` (else `content_id=`; an invalid `id=` is not retried with `content_id=`); the
  id is lowercased. Engine API `/server/api?method=get_media_files`, probe
  `/ace/getstream?id=&format=json&pid=`, stats `/ace/stat/…`, commands `/ace/cmd/…`; acexy
  `/ace/status?id=`. Play `<acexy_public>/ace/getstream?id=<id>`; play revision hashes
  `acexy_public`. **Observe works without acexy** (engine title + a reason; lib.rs:89,
  acestream.rs:189, test `without_acexy_there_are_no_checks_but_names_still_show`); checks need
  `acexy_control` (watcher guard). The probe lock (`probe_lock`) is per instance. Name clean-up
  and timing constants are in code.
- `extract.rs`: tool order fixed (yt-dlp then streamlink), argument arrays fixed in code. Cache and
  in-flight coalescing are keyed by page alone (extract.rs:74/76). Output parsers filter headers
  to User-Agent, Accept, Accept-Language, Referer, Origin (extract.rs:456). `classify` matches
  fixed English phrases and keeps parser/schema errors inconclusive (extract.rs:437); the final
  error prefers not-found, then no-stream, then the first failure (extract.rs:245). Stdout is
  discarded on non-zero exit (extract.rs:394). `expires_of` reads `expires`. Runner: argv without
  shell, null stdin, bounded drained pipes, deadline, process-group kill; inherits the environment
  plus `PYTHONDONTWRITEBYTECODE=1`.
- `relay.rs`: HLS relay keyed by page; refresh, probe, promote and invalidate use the page alone
  (relay.rs:168/200/325/341/406); every descriptor header is forwarded upstream (relay.rs:352/390);
  400/401/403/404/410 mean expired.
- `net.rs`: public-only resolver and per-hop redirect checks for public fetches (net.rs:145), the
  subprocess proxy resolves and checks every target (net.rs:229/260). Already rule-independent.
- Contract: `match_revision` must change when identity, ref or hints for the same input change;
  `play_revision` when the play reference for the same ref changes (gateway-contract lib.rs:49/51).
  lineup skips a resync when both are equal (server checker.rs:330) and counts pending channels by
  `play_revision` (server store/routes.rs:175).

## Target config

```toml
listen = "0.0.0.0:8099"

[extractors.yt-dlp]
command = "/opt/tools/bin/yt-dlp"
args = ["--ignore-config", "--no-playlist", "--no-warnings", "--dump-single-json",
        "--socket-timeout", "15", "--proxy", "{proxy}", "--", "{url}"]
output = "yt-dlp-json"          # yt-dlp-json | streamlink-json | descriptor-json
# not_found = [...], no_stream = [...]   # extra phrases on top of the format's defaults
# env = { XDG_CACHE_HOME = "/tmp/yt-dlp" }   # explicit additions to the scrubbed environment

[extractors.streamlink]
command = "/opt/tools/bin/streamlink"
args = ["--no-config", "--json", "--http-proxy", "{proxy}", "--", "{url}", "best"]
output = "streamlink-json"

[rules.vk]
kind = "relay"
title = "VK video"
input_hint = "vk.com, vkvideo.ru or vksport link to a video or broadcast"
extractors = ["yt-dlp", "streamlink"]   # tried in order
vars = { owner = "int", video = "pos" } # typed captures: canonical decimal, i64, video > 0
identity = "vk:{owner}_{video}"
ref = "https://vk.com/video{owner}_{video}{?list}"
group = "VK"
# expired_status = [400, 401, 403, 404, 410]   # defaults
# expires_param = "expires"

# Patterns are tried in order; the first match wins.
[[rules.vk.match]]                              # ?z=video… beats the path
hosts = ["vk.com", "vk.ru", "vkvideo.ru"]
query = [
  { param = "z",    re = 'video(?P<owner>-?[0-9]+)_(?P<video>[0-9]+)(?:[^0-9].*)?' },
  { param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true },
]

[[rules.vk.match]]
hosts = ["vk.com", "vk.ru", "vkvideo.ru"]
path = '^/video_ext\.php'
query = [
  { param = "oid",  re = '(?P<owner>-?[0-9]+)' },
  { param = "id",   re = '(?P<video>[0-9]+)' },
  { param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true },
  { param = "hash", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true },   # only if list is still empty
]

[[rules.vk.match]]
hosts = ["vk.com", "vk.ru", "vkvideo.ru"]
path = 'video/?(?P<owner>-?[0-9]+)_(?P<video>[0-9]+)'
query = [{ param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true }]

[[rules.vk.match]]                              # live-<o>_<v>: the dash is the owner's sign
hosts = ["vk.com", "vk.ru", "vkvideo.ru"]
path = 'live(?P<owner>-[0-9]+)_(?P<video>[0-9]+)'
query = [{ param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true }]

[[rules.vk.match]]                              # legacy vkserve proxy URLs, any other host
path = '/vk/(?P<owner>-?[0-9]+)_(?P<video>[0-9]+)'
query = [{ param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true }]

[[rules.vk.examples]]
input = "https://sport.vkvideo.ru/live-100001_200002?list=0a1b2c"
identity = "vk:-100001_200002"
ref = "https://vk.com/video-100001_200002?list=0a1b2c"

[[rules.vk.examples]]
input = "https://vk.com/video1_2?z=video3_4"
identity = "vk:3_4"

[[rules.vk.examples]]
input = "https://example.com/video-1_2"
match = false

[rules.acestream]
kind = "acestream"
title = "Acestream"
input_hint = "40-hex content id, acestream:// link or acexy/engine URL"
engine_control = "http://acestream:6878"
acexy_control = "http://acexy:8080"      # optional: watcher numbers in observe, the check guard
checks = true                            # opt-in; needs acexy_control; players must go through acexy
play = "http://media-host:8080/ace/getstream?id={id}"   # acexy, the engine, or any proxy
vars = { id = "lower" }
identity = "ace:{id}"
ref = "acestream://{id}"

[[rules.acestream.match]]
input = '(?i)^acestream://(?P<id>[0-9a-f]{40})/*$'

[[rules.acestream.match]]
input = '(?i)^(?P<id>[0-9a-f]{40})$'

[[rules.acestream.match]]
path = '/ace/'
query = [
  { param = "id",         re = '(?i)(?P<id>[0-9a-f]{40})', optional = true },
  { param = "content_id", re = '(?i)(?P<id>[0-9a-f]{40})', optional = true },
]
require = ["id"]

[[rules.acestream.examples]]
input = "ACESTREAM://41169A3F280A5537B0611B775E926852D372743D//"
identity = "ace:41169a3f280a5537b0611b775e926852d372743d"

[egress]
allow = []

[limits]
# unchanged
```

## Design

### Rule trait and registry

```rust
#[async_trait]
trait Rule: Send + Sync {
    fn info(&self) -> RuleInfo;
    async fn match_inputs(&self, inputs: &[String]) -> Vec<MatchResult>;
    async fn observe(&self, reference: &str) -> Observation;   // only if Observe is advertised
    async fn check(&self, reference: &str, force: bool) -> CheckResponse;  // only if Check is
    fn relay(&self) -> Option<Arc<RelayPolicy>> { None }        // serves /play/{name}.m3u8
}
```

- `Gateway { rules: IndexMap<String, Arc<dyn Rule>>, relay: Option<Relay>, engines: EngineLocks,
  _proxy }`. Handlers look the rule up by name; unknown name → existing `unknown_rule`. No rule
  name appears in code.
- Rule names: `[a-z0-9][a-z0-9_-]{0,31}`, validated at load (they appear in URLs and lineup routes).
- Every rule re-parses its own `ref` with the same pure matcher for play, observe and check, so a
  stored ref is self-contained after a restart (contract lib.rs:105). Load verifies for every
  example that `ref` matches again and renders the same identity/ref (round trip).
  Captures that end up in a ref therefore need the same validation in every pattern that can
  replay it (why `hash` uses the `list` check).

### Matching

- A rule's `match` list is tried in order; the first pattern that matches gives the variables.
- Pattern fields (all present ones must match):
  - `input`: regex on the trimmed raw input. Used for non-URL forms.
  - `hosts`: the URL's host equals one of them or ends with `.<host>`. Absent = any host.
  - `path`: regex searched in the URL path (unanchored unless the regex anchors it).
  - `query`: **ordered list** of `{param, re, optional?}`. `re` must match the decoded value in
    full. Duplicate keys: the first value counts (today's `query_pairs().find`). A required entry
    that is missing or invalid fails the pattern; an optional one that is invalid is dropped.
  - `require`: variables that must be non-empty after all fields (for "either of two params").
  - Patterns with `hosts`/`path`/`query` require an http(s) URL with a host. Userinfo is refused
    (new; today VK accepts it, intentional tightening).
- Variables come from named captures. Fields are evaluated in order (input, path, then query
  entries in list order) and the **first non-empty value wins**; this gives list-before-hash
  without relying on map order.
- `vars` types normalize captures before templates: `int` (signed decimal → i64, canonical, so
  `-001` → `-1`), `pos` (as `int`, must be > 0), `lower`, `str` (default). A failed conversion
  (overflow, zero for `pos`) fails the pattern, and the next pattern is tried.
- Regexes use the `regex` crate (linear time) with a compiled-size limit; inputs over 4 KiB are
  refused before matching.
- Built in for every relay rule, before its patterns: the gateway's own play URL
  (`…/play/{name}.m3u8?ref=<x>` on any host or prefix) matches `x` again, depth 1 (today it is
  recursive; intentional tightening).

### Templates

- `identity`, `ref`, `play` (acestream/link); relay play is fixed `play/{name}.m3u8?ref={ref}`.
- Dialect, defined here rather than inherited from RFC 6570:
  - `{var}` percent-encodes every byte outside the URI unreserved set (`A-Z a-z 0-9 - . _ ~`).
  - `{?a,b}` / `{&a}` add `name=value` pairs, values encoded as above, empty or missing variables
    skipped.
  - In `identity`, `{var}` is inserted raw: identities are opaque keys, never fetched.
  - Fetchable templates (`play`, and a `ref` that starts with `http://` or `https://`): scheme and
    authority must be literal text; a variable may not appear before the path (load error).
  - Opaque refs (any other literal scheme, such as `acestream://{id}`): variables may appear
    anywhere after the literal `scheme://`, still encoded as above. They are never fetched; the
    rule's own patterns parse them back.
  - Unknown variable = load error (checked against every pattern's capture names). Built-in:
    `{rule}`.
- Decoding happens once (query values are decoded when matched), encoding once (when rendered):
  a captured `a+b` renders as `a%2Bb`. Tests cover `/ % & + = ? #` and non-ASCII in tokens.

### Revisions

- Canonical form: the parsed rule config serialized deterministically (sorted keys, normalized
  regex/template strings), hashed with FNV-1a/64, short hex.
- `match_revision = hash(MATCHER_VERSION, kind, BACKEND_HINTS_VERSION[kind], match patterns, vars,
  identity, ref, title, group)`. `MATCHER_VERSION` is a code constant bumped when matching or
  template semantics change; `BACKEND_HINTS_VERSION` covers code-produced hints (acestream's name
  clean-up).
- `play_revision = hash(PLAY_VERSION, kind, play template or the relay path format,
  match_revision)`. Including `match_revision` covers play templates whose variables depend on the
  patterns; an extra play resync after a match change is harmless.
- So editing a rule or upgrading matcher code makes lineup resync it on the next poll.

### Backends (`kind`)

| kind | match hints | play | observe | check |
|---|---|---|---|---|
| `link` | `title`/`group` from config | `play` template | – | – |
| `relay` | `group` from config | `play/{name}.m3u8?ref=` through the shared relay | lookup + playlist summary | same, as today |
| `acestream` | engine name, cleaned (code), region, "Sports" | `play` template | engine title always; watcher numbers when `acexy_control` is set | opt-in, guarded probe (`checks = true` + `acexy_control`) |

- `link` is new and cheap; it covers "streams behind any proxy, keyed by an id" without code.
- `acestream` keeps its engine/acexy choreography in Rust. Only the play URL, identity, ref and
  input forms move to config. It requires a `{id}` variable (load error otherwise).
- **Engine locks.** `EngineLocks` maps a normalized `engine_control` origin (scheme, lowercased
  host, port) to one shared mutex. Every acestream rule pointing at that engine gets the same lock;
  the detached check task keeps owning it through guard, probe and stop, as today. Different
  hostnames for one engine are documented as "use one address per engine".
- The watcher guard only sees viewers playing through acexy. Docs say so next to `checks`.
  `force` still skips the guard explicitly and only when the caller asks.

### Relay and extraction state

- `RelayPolicy` (immutable, per relay rule): extractor chain with each extractor's config,
  error phrases, `expired_status`, `expires_param`. Its fingerprint is a hash of all of these.
- `LookupKey = (policy fingerprint, canonical ref)`. Used for the descriptor cache, in-flight
  coalescing, failure backoff, invalidation, recording promotion, playback, probes, playlist fetch
  and refresh. Two rules with identical policies share entries; different policies never do.
- Sessions store `Arc<RelayPolicy>` and the key; refresh and segment paths use the session's policy
  for expiry decisions.
- Global and unchanged: admission semaphores (16 playback + 8 background), `limits.sessions`,
  `limits.session_resources`, active-session eviction protection, the egress guard.

### Extractors

- `[extractors.<name>] command, args, output, timeout?, version_args? (default ["--version"]),
  not_found?, no_stream?, env?`. Args are an array (no shell); placeholders `{url}` and `{proxy}`
  only, substituted raw (never URL-encoded); `{proxy}` must appear in args (load error otherwise).
- Runner unchanged: no shell, null stdin, bounded concurrently drained stdout/stderr, deadline,
  process group killed on drop, detached coalesced work. The `--version` check at startup uses the
  same runner (bounded, scrubbed env) instead of lib.rs's separate helper.
- Environment: cleared, then `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR` passed
  through; `PYTHONDONTWRITEBYTECODE=1`; `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and their
  lowercase forms set to the gateway proxy; `NO_PROXY`/`no_proxy` unset; plus the extractor's
  explicit `env` (for example certificate bundle or XDG paths). Order: pass-through, then `env`,
  then the forced proxy variables last; `env` naming any proxy or bypass variable is a load error.
  Never passed: ambient proxies, `PYTHONPATH`/`PYTHONHOME`, loader variables, anything else.
- Trust boundary, documented: commands are operator-trusted code. The proxy flag and variables make
  compliant tools use the egress guard, but they are not an OS sandbox; a script can ignore them.
  Host-level egress rules (already recommended in docs/gateway.md) remain the real fence.
- Output formats (code): `yt-dlp-json`, `streamlink-json` (today's parsers), `descriptor-json`:
  `{"manifest_url": "...", "headers": {...}, "is_live": bool?, "title": str?, "duration": num?,
  "expires": unix?}` on exit 0, or `{"error": "...", "kind": "not_found" | "no_stream" |
  "failed"}` on exit 0 or non-zero (stdout is parsed for the error envelope even on failure; other
  formats keep today's stderr classification).
- **One validator for all formats** before anything is cached: manifest is an absolute http(s) URL
  with a host and no userinfo; headers filtered to today's allowlist (User-Agent, Accept,
  Accept-Language, Referer, Origin) with valid names and values; duration finite and ≥ 0;
  `expires` outside a sane range (before 2001 or after now + 10 years) is ignored, not rejected
  (it only ever shortens the cache lifetime). Public-only DNS and redirect checks stay at every
  actual fetch.
- Error classification: format defaults as today plus config phrases; `descriptor-json` uses its
  `kind`. Schema/parser errors stay inconclusive; fallback order and final-error precedence as today
  (not-found, then no-stream, then the first failure).
- Load errors: unknown extractor names, empty chains, zero or absurd timeouts and limits.

### Config self-test

- `[[rules.<name>.examples]]`: `input` plus expected `identity`/`ref`/`play` or `match = false`.
  Run at startup and by `lineup-gateway --check-config <file>`.
- Matching and rendering are a pure layer: examples and `--check-config` never start tools, the
  proxy, or engine/acexy calls (async hints are a separate step).
- Today's VK and Acestream parser tests become examples in `deploy/gateway.example.toml`, extended
  with the cases above (z precedence, `-001`, `1_02`, list/hash combinations including invalid list
  plus hash and an encoded hash, repeated keys, mixed-case ids and schemes). A test runs the
  example config's examples. Backend, relay, egress and subprocess tests stay as they are; examples
  add coverage, they replace only the parser unit tests.

## Acceptance tests (new)

- Two acestream rules on one engine: concurrent checks through both names, one cancelled, never
  more than one engine session open.
- Two relay rules accepting the same page with different extractor orders: separate descriptors,
  one rule's failure/backoff and invalidation leave the other untouched; custom `expires_param` and
  `expired_status` honoured in playlist, segment, probe and refresh paths.
- `descriptor-json` returning Cookie/Authorization/Host headers, a private or userinfo manifest URL,
  NaN duration: headers dropped or result rejected; private manifest/redirect/key targets refused.
- Extractor environment: a fake tool prints its environment; only the allowed variables appear.
- Acestream without `acexy_control`: observe gives the engine title and a reason; no checks.

## Migration

- Version 0.6.0 of lineup-gateway (contract stays 1). Old `[acestream]`, `[vk]`, `[tools]` sections
  are rejected with a message pointing at the migration section in docs/gateway.md.
- Same rule names as before in the example config → existing lineup routes keep working. Play URLs
  are unchanged for both rules (`play/vk.m3u8?ref=`; the acexy URL written as the `play`
  template), so only the revisions change (now hashes): lineup resyncs once and the stored URLs
  stay the same.
- Behaviour changes to list in the notes: Acestream checks opt-in (`checks = true`); `id`→`content_id`
  fallback; userinfo URLs refused; nested gateway play URLs resolved one level only; the legacy
  `/vk/<o>_<v>` form now also matches on VK hosts when no other pattern does (today VK hosts never
  take it); a `video_ext.php` `hash` must pass the same check as `list` (`[A-Za-z0-9_-]{1,64}`,
  today unfiltered), so every canonical ref replays to itself; inputs over 4 KiB are refused.
- Dockerfile.gateway: unchanged tools; the example config moves to the new format.

## Phases

1. Rule trait + registry, config parsing with rule names and kinds, `EngineLocks`, `RelayPolicy`
   and `LookupKey` through extractor and relay; port `acestream` and `vk` behind the trait with
   their current parsers (no behaviour change). Tests pass unchanged except for config fixtures.
2. Pure matcher (patterns, typed vars, ordered query entries) + templates + examples +
   `--check-config`; replace `vk.rs` parsing and acestream `parse_id`; hashed revisions.
3. Extractors as config: shared runner for version checks, env scrubbing, `descriptor-json`, the
   common descriptor validator, per-rule relay options; `link` kind; acestream `play` template and
   opt-in checks.
4. Docs: docs/gateway.md rewritten around kinds, patterns, templates and the extractor interface
   (including the trust boundary); example config with VK and Acestream; migration notes; README
   pointer.

## Open questions

None blocking. Later, independent: Acestream name clean-up lists in config; an HTTP probe for
`link` rules; a `lua` kind if a site ever needs per-request logic.

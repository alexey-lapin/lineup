# lineup-gateway

A gateway turns references that can't be played as they are into playable URLs, for lineup's
gateway routes. lineup-gateway is the reference gateway; it speaks gateway contract v1 (crate
`gateway-contract`).

Everything it knows about sites and services is configuration: each **rule** is a named block in
the config with a **kind**, patterns that recognise inputs, and templates for what it answers.
lineup routes name a rule (`lineup route add <name> --gateway <url> --rule <rule>`).

| Kind | Play URL | Observe | Check |
|---|---|---|---|
| `relay` | the gateway: extractors look the page up, the gateway relays its HLS | lookup + playlist (live, recording, or ended and not ready) | same, as an outcome |
| `link` | a template, pointing at any other service | – | – |
| `acestream` | a template (acexy, the engine, any proxy) | the engine's name; acexy's live numbers when `acexy_control` is set | opt-in guarded engine probe |

The example config, [deploy/gateway.example.toml](../deploy/gateway.example.toml), has a `vk`
relay rule (VK video pages), a commented-out `acestream` rule and a commented-out `link` rule.

## Run it

```sh
docker build -f Dockerfile.gateway -t lineup-gateway .
lineup-gateway --check-config --config gateway.toml      # or with the image: --entrypoint
docker run -d --name lineup-gateway -p 8099:8099 \
  -v $PWD/gateway.toml:/etc/lineup-gateway.toml:ro lineup-gateway
```

Then in lineup:

```sh
lineup route test --gateway http://gateway-host:8099      # lists the gateway's rules
lineup route add VK --gateway http://gateway-host:8099 --rule vk
```

Run it **outside any VPN** the players are not behind: signed stream URLs are often bound to the
address that looked them up. Players never see those URLs; only the gateway fetches from the CDN,
with the user agent the lookup used, so the URLs are not revoked by other clients.

## Rules

```toml
[rules.<name>]
kind = "relay" | "link" | "acestream"
title = "Shown in lineup"            # default: the name
input_hint = "What to paste"
group = "Suggested group"            # relay, link
identity = "vk:{owner}_{video}"      # de-duplication key in lineup
ref = "https://vk.com/video{owner}_{video}{?list}"   # the canonical, self-contained reference
vars = { owner = "int", video = "pos" }

[[rules.<name>.match]]               # patterns, tried in order
hosts = ["vk.com", "vk.ru"]          # the host or a subdomain
path = 'video/?(?P<owner>-?[0-9]+)_(?P<video>[0-9]+)'
query = [{ param = "list", re = '(?P<list>[A-Za-z0-9_-]{1,64})', optional = true }]

[[rules.<name>.examples]]            # checked on every load
input = "https://m.vk.com/video-1_2?list=abc"
identity = "vk:-1_2"
ref = "https://vk.com/video-1_2?list=abc"
```

Rule names are 1–32 of `a-z 0-9 - _`. A rule's name is part of lineup's routes: renaming a rule
is creating another one.

### Patterns

The first pattern that matches gives the variables. A pattern matches when every field it has
matches:

- `input`: a regex searched in the trimmed raw input. For ids and `scheme://` forms; anchor it
  (`^…$`) to match the whole input. It cannot be combined with the URL fields.
- `hosts`: the URL's host is one of them or a subdomain.
- `path`: a regex searched in the URL path.
- `query`: an ordered list of `{param, re, optional}`. The parameter's first value, decoded, must
  match `re` in full. A missing or non-matching required entry fails the pattern; an optional one
  is skipped.
- `require`: variables that must be non-empty (for "one of two parameters").

URL fields need an http(s) URL with a host and no user info. Inputs over 4 KiB are refused.

Variables are the regexes' named captures, `(?P<name>…)`. Path captures are percent-decoded once
(a `+` stays a `+`), query values are decoded as form values, and templates encode once, so a ref
reads back to itself. Fields are read in order (input, path, then query entries) and the first
non-empty value of a variable wins; so two query entries that
capture the same variable are a fallback (VK's `list`, then `hash`). `vars` normalises captures:
`int` (signed 64-bit, written canonically: `-001` → `-1`), `pos` (as `int`, above zero), `lower`.
A value that does not convert fails the pattern, and the next one is tried.

A relay rule also accepts its own play URLs (`…/play/<name>.m3u8?ref=…`, on any host or prefix)
and matches the `ref` inside, one level deep.

### Templates

- `{var}` percent-encodes every byte outside `A-Z a-z 0-9 - . _ ~`. In `identity` it is inserted
  as it is (identities are keys, never fetched).
- `{?a,b}` adds `?a=…&b=…`, `{&a}` adds `&a=…`; empty or missing variables are skipped.
- `{rule}` is the rule's name; no pattern may capture `rule`.
- Every `{var}` in `identity`, `ref` and `play` must be captured by every pattern, and an input
  whose substitution comes out empty is not accepted: a match always has a full identity and ref.
- Fetchable templates (`play`, and a `ref` whose scheme is http or https, in any case) keep their
  scheme and host literal: no variable before the path. Other refs are opaque references with a
  literal `scheme://` (such as `acestream://{id}`); the rule's own patterns read them back.

Every example's ref must be accepted again by the rule, with the same identity and ref: refs are
what lineup stores and sends back for play, observe and check.

### Revisions

A rule's `match_revision` and `play_revision` are hashes of its config (patterns, vars,
templates, title, group, play template) and of the gateway's matcher and play versions. lineup
polls them and revalidates a route's channels when they change, so editing a rule needs no
manual resync.

## Relay rules and extractors

```toml
[extractors.yt-dlp]
command = "/opt/tools/bin/yt-dlp"
args = ["--ignore-config", "--dump-single-json", "--proxy", "{proxy}", "--", "{url}"]
output = "yt-dlp-json"           # yt-dlp-json | streamlink-json | descriptor-json
# timeout_secs = 60              # default: limits.extraction_timeout_secs
# version_args = ["--version"]   # run at startup; the gateway does not start if it fails
# not_found = ["…"]              # extra phrases meaning the page has no video
# no_stream = ["…"]              # extra phrases meaning no playable stream yet
# env = { SSL_CERT_FILE = "/etc/ssl/cert.pem" }

[rules.vk]
kind = "relay"
extractors = ["yt-dlp", "streamlink"]   # tried in order
# expired_status = [400, 401, 403, 404, 410]   # upstream statuses meaning the URLs expired
# expires_param = "expires"                    # manifest query parameter with its expiry
```

`args` are passed without a shell; `{url}` is the page and `{proxy}` the egress proxy URL, both
required, both inserted as they are. Tools run in a cleared environment: `PATH`, `HOME`, `LANG`,
`LC_ALL`, `LC_CTYPE`, `TZ` and `TMPDIR` are passed through, then the extractor's `env`, then
`HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` (both cases) set to the egress proxy; `NO_PROXY` is
never set, and `env` may not name any of these.

### Your own extractor (`descriptor-json`)

Any program can be an extractor: it gets the page as an argument and prints one JSON object.

```json
{"manifest_url": "https://cdn.example/master.m3u8?sig=…",
 "headers": {"User-Agent": "…", "Referer": "…"},
 "is_live": true, "title": "…", "duration": 5400, "expires": 1767225600}
```

Only `manifest_url` is required: an http(s) HLS playlist URL with a host and no user info. Headers
other than User-Agent, Accept, Accept-Language, Referer and Origin are dropped; a duration that is
not a finite non-negative number and an expiry outside a sane range are ignored. To report a
failure, print `{"error": "message", "kind": "not_found" | "no_stream" | "failed"}` (any exit
status); `not_found` makes checks report the channel dead, `no_stream` means "not playable yet",
`failed` says nothing about the page.

The proxy argument and variables make well-behaved tools use the egress proxy, but they are not a
sandbox: an extractor is code you trust. Keep the host-level egress rules below.

### How relay playback works

- `GET /play/<rule>.m3u8?ref=<ref>` looks the page up with the rule's extractors, opens a relay
  session and returns the master playlist with every URI (variants, audio and subtitle
  renditions, keys, init sections, segments) rewritten to `s/<session>/<n>.<ext>`.
- While a broadcast is live the playlists are live; after it ends, opening the same URL again
  plays the recording, which is seekable. A session started on the live broadcast stays live until
  the player reopens the channel.
- Lookups are cached for 60 s; once a media playlist with `#EXT-X-ENDLIST` is seen for a lookup
  that did not say live, up to 3 h (never past the signed URL's expiry). Failures back off (1, 2,
  5, 10 min). An empty ENDLIST playlist (recording still processing) keeps the short lifetime.
  Lookups are cached and coalesced per rule settings (extractors, expiry options) and page, so two
  rules never share a lookup or its failures. Playback admits 16 lookups and background work
  (observe, check) 8 more of its own, and background work runs in one slot, so it never refuses or
  blocks playback.
- An upstream status in the rule's `expired_status` means the signed URLs expired (some CDNs
  answer 400). The lookup is dropped. A media playlist is refreshed once per session (concurrent
  requests share one refresh), and only onto the new master's playlist with exactly the same
  definition (variant attributes or rendition tag) whose timeline provably continues: same
  live/recorded state, target duration and init sections, a media sequence inside the window
  already served with every overlapping segment of exactly the same duration and discontinuity
  number, and never a growing playlist continued as a finished one. Otherwise, and for segments,
  the player gets 410 and its next open starts afresh (on the recording, once the broadcast has
  ended).
- Status: ENDLIST with segments and a lookup that is not live is a recording; ENDLIST from a lookup
  that still says live, or without segments, is "ended, recording not ready" (inconclusive in
  checks). Only explicit "video not found/removed/private" messages from the extractors count as a
  dead video; parser and network errors are inconclusive.
- Range requests are passed through to segments (one range only); nothing else from the player is
  forwarded.
- Refused: LL-HLS parts and hints, content steering, variable substitution, session data URIs,
  any other tag carrying a URI (for example image playlists), and keys other than plain AES-128. No
  transcoding: DASH-only or MP4-only pages fail clearly.
- Sessions: at most `limits.sessions`; idle ones are evicted, but a session used within the last
  minute never is, so a new viewer gets 503 instead of stopping an active one. HEAD opens no session.
- Upstream requests carry only the lookup's User-Agent, Accept, Accept-Language, Referer and
  Origin, for every host of the stream. Cookies and authorization are never sent: sites whose
  media needs them are not supported.

## Acestream rules

```toml
[rules.acestream]
kind = "acestream"
engine_control = "http://acestream:6878"     # gateway -> engine (names, checks)
acexy_control = "http://acexy:8080"          # optional: live numbers and the check guard
play = "http://media-host:8080/ace/getstream?id={id}"   # as players reach the stream
checks = true                                # optional, needs acexy_control
```

The patterns must capture `id` (see the example config for ids, `acestream://` links and
acexy/engine URLs). Matching asks the engine for the content's name: quality words are dropped, a
two-letter `[DE]` tag becomes the region hint, sports-looking names get the group "Sports".

Observing a channel shows the engine's own name and, while acexy serves the stream, its live
numbers; it never starts a stream.

Checks start a short engine session of their own, read its statistics and stop it. Before each,
acexy is asked whether the stream is watched; a watched stream is skipped, and an unreadable guard
skips too. The guard only sees players that go through acexy, so turn checks on only when the
`play` template points at acexy. One probe runs per engine at a time, across every rule that uses
the engine (use one address per engine); a probe that was started always stops its own session,
even when lineup stops waiting.

## Network boundary

- Every public fetch (extractors and the relay) refuses private, loopback, link-local, CGNAT,
  unique-local, multicast and metadata addresses, on every connection and redirect. Extractors are
  pointed at an in-process proxy that applies this rule. A matching host in a rule is never
  permission to reach a private address.
- Acestream rules talk only to their configured `engine_control` and `acexy_control` origins and
  never follow redirects; URLs the engine reports are validated and rebased onto the engine.
- Ambient `HTTP(S)_PROXY` settings are ignored, so no proxy can resolve names past this rule.
- Not shipped, recommended on the host: drop the container's outbound traffic to private ranges
  except DNS and the engine/acexy host:port pairs, for example with `DOCKER-USER` iptables rules.
- No authentication: run it on a trusted LAN only.

## Health and shutdown

`lineup-gateway --healthcheck` asks the configured port's `/healthz` (the image's HEALTHCHECK uses
it). `lineup-gateway --check-config` loads the config, compiles every rule and checks its
examples, without starting anything. SIGTERM and SIGINT stop the server gracefully. At startup
each extractor a rule uses must answer its `version_args`, or the gateway does not start.

## Updating the extractors

yt-dlp and streamlink are pinned in `Dockerfile.gateway`. To update: bump both pins, rebuild, and
smoke-test a live and a recorded video of each relay rule:

```sh
curl -s "http://localhost:8099/play/vk.m3u8?ref=<urlencoded link>" | head
ffprobe -v error -show_entries stream=codec_type,width,height "http://localhost:8099/play/vk.m3u8?ref=<…>"
```

## Migrating from 0.5

The 0.5 config format (`[acestream]`, `[vk]`, `[tools]`) is refused at startup. Start from the
example config:

- `[tools]` → `[extractors.yt-dlp]` and `[extractors.streamlink]` as in the example.
- `[vk]` → the example's `[rules.vk]` block (patterns and examples included).
- `[acestream]` → the example's `[rules.acestream]` block: `engine_control` and `acexy_control`
  as before; `acexy_public = "http://host:8080"` becomes
  `play = "http://host:8080/ace/getstream?id={id}"`; add `checks = true` to keep checks.

The rule names and play URLs stay the same, so lineup routes keep working. The rule revisions
change once (they are hashes now), and lineup revalidates the routes on its next poll.

Behaviour changes:

- Acestream checks are off unless `checks = true`.
- An Acestream URL whose `id=` is not a valid id falls back to `content_id=`.
- URLs with user info are refused; inputs over 4 KiB are refused.
- A gateway play URL inside a gateway play URL is read one level deep only.
- VK: a `video_ext.php` `hash` must look like a `list` token (`[A-Za-z0-9_-]{1,64}`), so every
  stored ref reads back to itself; the legacy `/vk/<owner>_<video>` form is also accepted on VK
  hosts.

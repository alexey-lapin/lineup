# lineup-gateway

A gateway turns references that can't be played as they are into playable URLs, for lineup's
gateway routes. lineup-gateway is the reference gateway; it speaks gateway contract v1 (crate
`gateway-contract`) and has two rules:

| Rule | Accepts | Plays from | Checks |
|---|---|---|---|
| `acestream` | 40-hex ids, `acestream://` links, acexy/engine URLs | acexy, directly (`acexy_public`) | engine probe with the acexy activity guard |
| `vk` | vk.com / vk.ru / vkvideo.ru video and broadcast links (also `sport.vkvideo.ru/live-…`), old vkserve URLs | the gateway, which relays HLS | lookup + playlist (live, recording, or ended and not ready) |

## Run it

```sh
docker build -f Dockerfile.gateway -t lineup-gateway .
docker run -d --name lineup-gateway -p 8099:8099 \
  -v $PWD/gateway.toml:/etc/lineup-gateway.toml:ro lineup-gateway
```

Configuration: [deploy/gateway.example.toml](../deploy/gateway.example.toml). Then in lineup:

```sh
lineup route test --gateway http://media-host:8099
lineup route add VK --gateway http://media-host:8099 --rule vk
lineup route add Acestream --gateway http://media-host:8099 --rule acestream
```

Run it **outside any VPN** that the players are not behind: VK's signed stream URLs are bound to
the address that looked them up. Players never see those URLs; only the gateway fetches from the
CDN, with the user agent the lookup used, so the URLs are not revoked by other clients.

## How VK playback works

- `GET /play/vk.m3u8?ref=<vk.com link>` looks the page up with yt-dlp (streamlink as the fallback),
  opens a relay session and returns the master playlist with every URI (variants, audio and subtitle
  renditions, keys, init sections, segments) rewritten to `s/<session>/<n>.<ext>`.
- While the broadcast is live the playlists are live; after it ends, opening the same URL again
  plays the recording, which is seekable. A session started on the live broadcast stays live until
  the player reopens the channel.
- Lookups are cached for 60 s; once a media playlist with `#EXT-X-ENDLIST` is seen for a lookup
  that did not say live, up to 3 h (never past the signed URL's expiry). Failures back off (1, 2,
  5, 10 min). Lookups are coalesced per page, at most 16 are admitted at once, and background
  work (observe, check) uses one slot so playback always has one.
- An upstream 400/401/403/404/410 means the signed URLs expired (VK answers 400). The lookup is
  dropped. A media playlist is refreshed once per session, and only onto the new master's playlist
  with exactly the same definition (variant attributes or rendition tag); otherwise, and for
  segments, the player gets 410 and its next open starts afresh.
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

## Network boundary

- Every public fetch (yt-dlp, streamlink and the relay) refuses private, loopback, link-local,
  CGNAT, unique-local, multicast and metadata addresses, on every connection and redirect. The
  extractors are forced through an in-process proxy that applies this rule.
- The Acestream rule talks only to its configured `engine_control` and `acexy_control` origins and
  never follows redirects; URLs the engine reports are validated and rebased onto the engine.
- Ambient `HTTP(S)_PROXY` settings are ignored, so no proxy can resolve names past this rule.
- Not shipped, recommended on the host: drop the container's outbound traffic to private ranges
  except DNS and the engine/acexy host:port pairs, for example with `DOCKER-USER` iptables rules.
- No authentication: run it on a trusted LAN only.

## Acestream checks

Checks need `acestream.acexy_control`: only acexy can tell whether someone is watching. Without it
the rule offers no checks (also not forced ones). One probe runs per engine at a time; a probe that
was started always stops its own engine session, even when lineup stops waiting. Observing a
channel shows the engine's own name and, while acexy serves the stream, its live numbers.

## Health and shutdown

`lineup-gateway --healthcheck` asks the configured port's `/healthz` (the image's HEALTHCHECK uses
it). SIGTERM and SIGINT stop the server gracefully. At startup each configured tool must answer
`--version`, or the gateway does not start.

## Updating the extractors

yt-dlp and streamlink are pinned in `Dockerfile.gateway`. To update: bump both pins, rebuild, and
smoke-test a live and a recorded VK video:

```sh
curl -s "http://localhost:8099/play/vk.m3u8?ref=<urlencoded vk.com link>" | head
ffprobe -v error -show_entries stream=codec_type,width,height "http://localhost:8099/play/vk.m3u8?ref=<…>"
```

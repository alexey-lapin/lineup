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
- Lookups are cached: 60 s for live broadcasts, up to 3 h for recordings (never past the signed
  URL's expiry); failures back off (1, 2, 5, 10 min). An upstream 401/403/404/410 drops the lookup
  and answers 410, so the player's next open starts afresh.
- Range requests are passed through to segments (one range only); nothing else from the player is
  forwarded.
- Refused: LL-HLS parts and hints, content steering, session data URIs, keys other than plain
  AES-128. No transcoding: DASH-only or MP4-only pages fail clearly.

## Network boundary

- Every public fetch (yt-dlp, streamlink and the relay) refuses private, loopback, link-local,
  CGNAT, unique-local, multicast and metadata addresses, on every connection and redirect. The
  extractors are forced through an in-process proxy that applies this rule.
- The Acestream rule talks only to its configured `engine_control` and `acexy_control` origins and
  never follows redirects; URLs the engine reports are validated and rebased onto the engine.
- Defence in depth on the host: drop the container's outbound traffic to private ranges except DNS
  and the engine/acexy host:port pairs, for example with `DOCKER-USER` iptables rules.
- No authentication: run it on a trusted LAN only.

## Updating the extractors

yt-dlp and streamlink are pinned in `Dockerfile.gateway`. To update: bump both pins, rebuild, and
smoke-test a live and a recorded VK video:

```sh
curl -s "http://localhost:8099/play/vk.m3u8?ref=<urlencoded vk.com link>" | head
ffprobe -v error -show_entries stream=codec_type,width,height "http://localhost:8099/play/vk.m3u8?ref=<…>"
```

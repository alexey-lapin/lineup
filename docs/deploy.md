# Deploying lineup

## Run it

lineup is published as a container image, `ghcr.io/alexey-lapin/lineup`, for amd64 and arm64. The example [compose file](../deploy/compose.yaml) runs it on port 9300 and keeps the database in the named volume `lineup-data`:

```sh
docker compose -f deploy/compose.yaml up -d
```

Pin a release tag rather than `latest`. The container runs as uid 10001 and stores everything under `/data`.

Open `http://<host>:9300` and set the base URLs under **Settings**. On a phone, add the page to the home screen to use it as an app. Over plain HTTP it works without offline support; served over HTTPS, it also caches the UI. Use addresses that Jellyfin and your players can reach, not `localhost`:

| Setting | What it is | Example |
|---|---|---|
| `acexy_base` | acexy, which plays Acestream ids | `http://media-host:8080` |
| `engine_base` | the Acestream engine, used for name lookups and (later) health checks | `http://media-host:6878` |
| `vk_base` | a VK proxy that serves `/vk/{owner}_{video}[?list=…]` | `http://media-host:8099` |

Each has a **Test** button (or `lineup settings test acexy|engine|vk [--base URL]`) that checks, from the lineup server, that the service answers: acexy's status endpoint, the engine's version, or any HTTP answer from the VK proxy. It tests the URL as typed, before you save it. A passing test does not prove Jellyfin or a phone can reach the address, only that lineup can.

## Point the CLI at it

```sh
mkdir -p ~/.config/lineup
echo 'server = "http://media-host:9300"' > ~/.config/lineup/config.toml
lineup status
```

Or set `LINEUP_SERVER`, or pass `--server`. Every command takes `--json` for scripts and agents, and exit codes are stable (0 ok, 2 usage, 3 not found, 4 conflict, 5 server unreachable).

Shell completions:

```sh
lineup completions zsh > ~/.zfunc/_lineup     # also bash, fish, elvish, powershell
lineup usage                                  # a usage spec (https://usage.jdx.dev)
```

CLI binaries for macOS and Linux (x86_64 and arm64) are attached to each GitHub release.

## Bring in an existing playlist

If you already have an M3U file, import it. Preview first; nothing is written until the second command.

```sh
lineup import ./channels.m3u -p jellyfin --dry-run
lineup import ./channels.m3u -p jellyfin
```

Acestream and VK links are recognised in any of their usual forms (acexy or engine URLs, `acestream://` ids, vk.com and vkvideo.ru links, proxy URLs). Anything else is kept as a raw URL. Re-importing is safe: channels that already exist are reused and their details are kept, and the report shows where the file differs.

## Connect Jellyfin

In Jellyfin, go to **Dashboard → Live TV → Tuner Devices → Add → M3U Tuner** and enter the playlist URL:

```
http://media-host:9300/p/jellyfin.m3u
```

`lineup playlist url jellyfin` prints it. The channel number in Jellyfin is the position in the playlist, so reordering in lineup reorders the guide.

If you replace an existing tuner, Jellyfin may treat the channels as new, because channel ids depend partly on the tuner URL. Favourites can reset.

## Guide data

Add one or more XMLTV sources (plain or gzipped) under **EPG** in the UI, or with the CLI:

```sh
lineup epg sources add https://example.org/guide.xml.gz
lineup epg sources ls          # last fetch status, channel count
lineup epg search "sports one" # find the tvg-id for a channel
```

lineup downloads each source shortly after it is added, 3 minutes after every start, and then every `epg_refresh_hours` (default 6, `0` turns the schedule off). `lineup epg fetch` fetches now. When several sources carry the same channel id, the one with the higher priority wins.

Each playlist gets its own guide with just its channels:

```
http://media-host:9300/p/jellyfin/epg.xml
```

In Jellyfin, go to **Dashboard → Live TV → TV Guide Data Providers → Add → XMLTV** and enter that URL. A channel gets guide data when its tvg-id matches a channel id in one of the sources. Channels marked as events get placeholder programmes ("Pre-match", the event, then "ended") built from their kickoff time, so they always show up in the guide.

Jellyfin keeps a downloaded guide for about an hour, so guide changes can take that long to appear.

## Refresh Jellyfin automatically

A Jellyfin hook runs Jellyfin's **Refresh Guide** task whenever a playlist or guide data changes, so new channels show up without waiting for Jellyfin's own schedule. Create an API key in Jellyfin (**Dashboard → API Keys**), then add the hook under **Hooks** in the UI, or:

```sh
lineup hook add jellyfin Jellyfin --url http://jellyfin:8096 --api-key <key>
lineup hook test 1          # really starts a guide refresh
lineup hook deliveries 1
```

Changes are collected for 30 seconds (`--debounce`) and delivered together. Because Jellyfin reuses a downloaded guide for about an hour, the hook runs a second refresh 65 minutes after the last change (`--followup-minutes`, 0 turns it off). The API key is stored write-only and never shown again.

A webhook hook sends the same events as JSON to any URL (`lineup hook add webhook <name> <url>`):

```json
{"events": [{"type": "playlist.changed", "at": "…", "payload": {"slugs": ["jellyfin"]}}],
 "window_start": "…", "window_end": "…"}
```

## Health checks

lineup can check whether each Acestream channel's swarm has peers, through the engine (`engine_base`). It starts a short session, reads its statistics and stops it again. Before each check it asks acexy whether anyone is watching that stream, and skips it if so.

```sh
lineup probe "Sports One"   # one channel, waits for the result
lineup probe                # all of them, in the background
lineup ls --health dead
```

Scheduled checks are off by default; set `probe_interval_hours` (Settings, or `lineup settings set probe_interval_hours 6`) to turn them on. Hooks can subscribe to `channel.unhealthy` and `channel.recovered`. A check measures the swarm, not whether playback works.

## Logs

`docker logs lineup` shows, at the default `LINEUP_LOG=info`:

- every change event (`event playlist.changed {"slugs":["jellyfin"]}`, `epg.updated`, …);
- each guide fetch per source (updated with channel and programme counts, not modified, or the error), and a summary of each scheduled run;
- each hook delivery, successful or not;
- health-check runs and each channel's result;
- every change made through the API, and every playlist or guide download (`192.168.1.50 GET /p/jellyfin.m3u 200 3 ms`), so you can see when Jellyfin re-reads them.

`LINEUP_LOG=debug` adds every other request.

## Backups

```sh
lineup backup ~/backups/lineup-$(date +%F).db
```

This asks the server for a consistent SQLite snapshot (`VACUUM INTO`). The result is a single self-contained file, with no WAL to carry along. The command refuses to overwrite an existing file, and checks that what it received is a SQLite database.

To restore, run [`deploy/restore.sh`](../deploy/restore.sh) on the Docker host, from the repository root (or wherever you copied the script):

```sh
docker stop lineup && sh deploy/restore.sh ./lineup-backup.db && docker start lineup
```

It checks the backup (`integrity_check` plus a test query) before changing anything. It stages the copy next to the live database, then moves the current `lineup.db`, `-wal` and `-shm` into `/data/pre-restore-<timestamp>/` (WAL can hold committed rows, so nothing is deleted). It refuses to run while lineup is up. If validation fails, the live files are untouched, and because `&&` chains the commands, lineup stays stopped until the restore succeeds.

Re-importing an exported M3U is **not** a full restore. It brings back that playlist's channels and their M3U fields, but not disabled channels, other playlists, settings, event flags or kickoff times. Use the database backup for that.

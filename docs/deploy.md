# Deploying lineup

## Run it

lineup is published as a container image, `ghcr.io/alexey-lapin/lineup`, for amd64 and arm64. The example [compose file](../deploy/compose.yaml) runs it on port 9300 and keeps the database in the named volume `lineup-data`:

```sh
docker compose -f deploy/compose.yaml up -d
```

Pin a release tag rather than `latest`. The container runs as uid 10001 and stores everything under `/data`.

Open `http://<host>:9300` and set the base URLs under **Settings**. On a phone, add the page to the home screen to use it as an app. Over plain HTTP it works without offline support; served over HTTPS, it also caches the UI. Use addresses that your players and media server can reach for acexy and the VK proxy, not `localhost`; the engine only needs to be reachable from lineup:

| Setting | What it is | Example |
|---|---|---|
| `acexy_base` | acexy, which plays Acestream ids | `http://media-host:8080` |
| `engine_base` | the Acestream engine, used for name lookups and health checks | `http://media-host:6878` |
| `vk_base` | a VK proxy that serves `/vk/{owner}_{video}[?list=…]` | `http://media-host:8099` |

Each has a **Test** button (or `lineup settings test acexy|engine|vk [--base URL]`) that checks, from the lineup server, that the service answers: acexy's status endpoint, the engine's version, or any HTTP answer from the VK proxy. It tests the URL as typed, before you save it. A passing test does not prove your players can reach the address, only that lineup can.

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
lineup import ./channels.m3u -p tv --dry-run
lineup import ./channels.m3u -p tv
```

Acestream and VK links are recognised in any of their usual forms (acexy or engine URLs, `acestream://` ids, vk.com and vkvideo.ru links, proxy URLs). Anything else is kept as a raw URL. Re-importing is safe: channels that already exist are reused and their details are kept, and the report shows where the file differs.

## Use the playlist

Each playlist is a plain M3U file at a fixed URL:

```
http://media-host:9300/p/tv.m3u
```

`lineup playlist url tv` prints it. Give it to anything that reads an M3U playlist from a URL: Jellyfin or Emby as an M3U tuner, Kodi with the IPTV Simple add-on, or an IPTV app such as TiviMate. VLC can play it too, without a guide. Plex can't read M3U directly; it needs a tuner bridge such as Threadfin in between.

lineup writes each channel's position in the playlist as `tvg-chno`. Whether a client numbers and orders channels by it depends on the client and its settings.

### Jellyfin

Go to **Dashboard → Live TV → Tuner Devices → Add → M3U Tuner** and enter the playlist URL. If you replace an existing tuner, Jellyfin may treat the channels as new, because channel ids depend partly on the tuner URL. Favourites can reset.

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
http://media-host:9300/p/tv/epg.xml
```

Add it wherever your player takes an XMLTV guide; in Jellyfin that is **Dashboard → Live TV → TV Guide Data Providers → Add → XMLTV**. A channel gets guide data when its tvg-id matches a channel id in one of the sources. Channels marked as events get placeholder programmes ("Pre-match", the event, then "ended") built from their kickoff time, so they always show up in the guide.

Players cache the guide (Jellyfin for about an hour), so guide changes can take a while to appear.

## Tell players about changes: hooks

Players and media servers re-read the playlist and guide on their own schedule. A hook makes lineup call a URL as soon as something changes, typically a media server's refresh endpoint:

```sh
lineup hook add <name> <url> [--method POST|PUT|PATCH] [--header Name=value]... [--events a,b] [--debounce 30]
lineup hook test 1          # sends a test event; the receiver really acts on it
lineup hook deliveries 1
lineup hook edit 1 --url <new url>
```

Changes are collected for 30 seconds (`--debounce`) and sent together, with retries. The body is always this JSON envelope, which a receiver may use or ignore:

```json
{"events": [{"type": "playlist.changed", "at": "…", "payload": {"slugs": ["tv"]}}],
 "window_start": "…", "window_end": "…"}
```

Header values (tokens) are stored write-only and never shown again. `lineup hook edit` keeps them unless you pass `--header`.

A successful delivery means the receiver accepted the call, not that it has finished refreshing.

### Jellyfin

Jellyfin's **Refresh Guide** task re-reads tuners and guide providers. To run it on every change:

1. Create an API key: **Dashboard → API Keys**.
2. Find the task's id:

   ```sh
   curl -s -H 'Authorization: MediaBrowser Token="<key>"' http://jellyfin:8096/ScheduledTasks \
     | jq -r '.[] | select(.Key == "RefreshGuide") | .Id'
   ```

3. Add the hook:

   ```sh
   lineup hook add Jellyfin http://jellyfin:8096/ScheduledTasks/Running/<task id> \
     --header 'Authorization=MediaBrowser Token="<key>"' --events playlist.changed,epg.updated
   ```

If the task id ever changes (a 404 in `lineup hook deliveries`), look it up again and `lineup hook edit <id> --url …`. Jellyfin reuses a downloaded XMLTV guide for a while (about an hour), so a refresh right after a guide change may still show the old programmes until a later refresh.

### Upgrading from 0.2

0.3.0 replaced the built-in Jellyfin hook with webhooks. Existing Jellyfin hooks are converted on first start into the webhook above, keeping the API key. A hook that had no task id stored (the default, since 0.2 looked it up itself) is converted **disabled**, with `REFRESH_GUIDE_TASK_ID` in its URL; lineup logs a warning about it at every start. Look up the task id as above, then `lineup hook edit <id> --url http://jellyfin:8096/ScheduledTasks/Running/<task id>` and `lineup hook enable <id>` (or edit it under **Hooks**). The 65-minute follow-up refresh is gone.

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

- every change event (`event playlist.changed {"slugs":["tv"]}`, `epg.updated`, …);
- each guide fetch per source (updated with channel and programme counts, not modified, or the error), and a summary of each scheduled run;
- each hook delivery, successful or not;
- health-check runs and each channel's result;
- every change made through the API, and every playlist or guide download (`192.168.1.50 GET /p/tv.m3u 200 3 ms`), so you can see when your players re-read them.

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

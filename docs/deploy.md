# Deploying lineup

## Run it

lineup is published as a container image, `ghcr.io/alexey-lapin/lineup`, for amd64 and arm64. The example [compose file](../deploy/compose.yaml) runs it on port 9300 and keeps the database in the named volume `lineup-data`:

```sh
docker compose -f deploy/compose.yaml up -d
```

Pin a release tag rather than `latest`. The container runs as uid 10001 and stores everything under `/data`.

Open `http://<host>:9300`. On a phone, add the page to the home screen to use it as an app. Over plain HTTP it works without offline support; served over HTTPS, it also caches the UI.

## Set up integrations

Integrations are the services your channels play through, set up under **Integrations** (or with `lineup integration set`). A new installation starts with Acestream and VK off: their channels can be added and imported, but stay out of playlists and guides until their integration is set up. Direct URLs need nothing.

| Integration | Address | What it is | Example |
|---|---|---|---|
| Acestream | Engine URL (required) | the Acestream engine: name lookups, health checks, and playback when acexy is not set | `http://media-host:6878` |
| Acestream | acexy URL (recommended) | [acexy](https://github.com/Javinator9889/acexy) in front of the engine: playback (several viewers share one engine session), and what health checks and live details need | `http://media-host:8080` |
| VK proxy | URL | a VK proxy (a thin layer over streamlink) serving `/vk/{owner}_{video}[?list=…]` | `http://media-host:8099` |

Playlists send players to acexy (or to the engine, without acexy) and to the VK proxy, so use addresses that your players and media server can reach, not `localhost`.

```sh
lineup integration set acestream --engine-url http://media-host:6878 --acexy-url http://media-host:8080
lineup integration set vk --url http://media-host:8099
lineup integration ls
lineup integration test acestream            # every configured part; or: test acestream engine --url URL
```

Each address has a **Test** button that checks, from the lineup server, that the service answers: the engine's version, acexy's status (with its active stream count), or any HTTP answer from the VK proxy. It tests the URL as typed, before you save it; the card also shows the live status of the saved addresses. A passing test does not prove your players can reach the address, only that lineup can.

Clearing the engine URL (or the VK URL) turns that integration off; its channels keep their playlists and positions and come back when it is set again. Clearing acexy moves Acestream playback to the engine and turns health checks off.

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

Acestream and VK links are recognised in any of their usual forms (acexy or engine URLs, also behind a path prefix, `acestream://` ids, vk.com and vkvideo.ru links, proxy URLs). Anything else is kept as a direct URL. Re-importing is safe: channels that already exist are reused and their details are kept, and the report shows where the file differs.

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

## Upgrading from 0.3

lineup 0.4 starts from a new database and cannot open a 0.3 one: it refuses to start and leaves the old file untouched. To move over:

1. Before upgrading, while 0.3 is still running, save what you will need (into an empty folder):

   ```sh
   lineup --json ls > channels.json                 # every channel, also disabled ones and ones in no playlist
   lineup --json playlist ls > playlists.json
   for p in $(jq -r '.[].slug' playlists.json); do
     curl -fsS -o "$p.m3u" "http://media-host:9300/p/$p.m3u"
     lineup --json playlist show "$p" > "playlist-$p.json"
   done
   lineup --json epg sources ls > epg-sources.json
   lineup --json hook ls > hooks.json               # header secrets are masked: take the Jellyfin API key from Jellyfin
   lineup --json settings get > settings.json       # the 0.3 base URLs
   ```

   The M3U files are what you import. The JSON files are records to recreate what an M3U does not carry: disabled channels (left out of the M3U), channels in no playlist, event flags and kickoff times.
2. Stop and remove the 0.3 container, and keep its volume as the backup. Only the 0.3 binary can read it, so keep the 0.3 image tag too if you might roll back.
3. Start 0.4 on a new, empty volume. With the example compose file, change the volume's `name:` (for example to `lineup-data-0.4`) and the image tag, then `docker compose up -d`. Starting 0.4 on the old volume fails with a message saying so, and changes nothing. (`deploy/restore.sh` uses the volume `lineup-data` unless you pass `VOLUME=lineup-data-0.4`.)
4. Set up the integrations (above), preview each import, then import it (`lineup import tv.m3u -p tv --dry-run`, then without `--dry-run`). Acestream and VK channels are recognised from their stream URLs, also behind a reverse-proxy path prefix (`http://proxy/acexy/ace/getstream?id=…`); the preview shows each entry's kind, so check that none you expect as Acestream or VK shows as direct.
5. From the JSON records, add back the channels in no playlist, disable the ones that were disabled, and set event flags and kickoff times. Then add the guide sources and hooks again.

Imported channels take their name, group, tvg-id and logo from the M3U.

## Health checks

lineup can check whether each Acestream channel's swarm has peers, through the engine. It starts a short session, reads its statistics and stops it again. Before each check it asks acexy whether anyone is watching that stream, and skips it if so. Checks therefore need acexy: without it they are unavailable (also with `--force`). acexy only knows about its own players, so a player connected to the engine directly can still be interrupted.

Clicking a channel opens its details: the engine's original name, and while someone watches through acexy, live peers, speeds and players (read from acexy's session; opening it never starts a stream), plus the latest check and **Check now**. `lineup info <channel>` prints the same.

```sh
lineup probe "Sports One"   # one channel, waits for the result
lineup probe                # all of them, in the background
lineup ls --health dead
```

Scheduled checks are off by default; set the schedule on the Acestream card (or `lineup integration set acestream --probe-interval 6`) to turn them on. Hooks can subscribe to `channel.unhealthy` and `channel.recovered`. A check measures the swarm, not whether playback works.

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

A backup restores into the same lineup version line: 0.4 refuses a 0.3 database (see Upgrading from 0.3).

Re-importing an exported M3U is **not** a full restore. It brings back that playlist's channels and their M3U fields, but not disabled channels, other playlists, integrations, settings, event flags or kickoff times. Use the database backup for that.

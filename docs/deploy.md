# Deploying lineup

## Run it

lineup is published as a container image, `ghcr.io/alexey-lapin/lineup`, for amd64 and arm64. The example [compose file](../deploy/compose.yaml) runs it on port 9300 and keeps the database in the named volume `lineup-data`:

```sh
docker compose -f deploy/compose.yaml up -d
```

Pin a release tag rather than `latest`. The container runs as uid 10001 and stores everything under `/data`.

Open `http://<host>:9300`. On a phone, add the page to the home screen to use it as an app. Over plain HTTP it works without offline support; served over HTTPS, it also caches the UI.

## Set up routes

A route is how channels are played, set up under **Routes** (or with `lineup route`). A new installation has one route, **Direct**, for stream URLs that are put in playlists as they are. Anything that needs work before it plays (an Acestream id, a video page) goes through a **gateway**: a separate service that speaks lineup's gateway contract, such as lineup-gateway. Add a gateway route for each of the gateway's rules you use:

```sh
lineup route test --gateway http://media-host:8099              # lists the gateway's rules
lineup route add Acestream --gateway http://media-host:8099 --rule acestream
lineup route add VK --gateway http://media-host:8099 --rule vk
lineup route test VK --input "https://vk.com/video-1_2"          # try an input without saving
lineup route ls
```

Playlists send players to the gateway (or to the service it points at, such as acexy for Acestream), so use addresses that your players and media server can reach, not `localhost`. Where the gateway points players is the gateway's configuration, not lineup's.

When you add a channel you pick its route, and the input is validated with that route only: the gateway answers with the canonical source, a play URL and suggestions (a title, a group). If the gateway is down, adding, editing and importing channels of its routes fail with a gateway error (CLI exit code 6); existing playlists and guides keep working, because lineup never calls a gateway to serve them.

A route can be disabled: its channels keep their playlists and positions but are left out of playlists and guides until it is enabled again. Its kind and rule never change; create another route and move the channels (`lineup edit <channel> --route <route>`). Changing a gateway route's address (`lineup route edit VK --base-url …`) revalidates every channel with the new address first, and changes nothing if any fails. `lineup route resync <route>` revalidates every channel with the gateway now; lineup also does that on its own when the gateway reports that its rule changed.

## Point the CLI at it

```sh
mkdir -p ~/.config/lineup
echo 'server = "http://media-host:9300"' > ~/.config/lineup/config.toml
lineup status
```

Or set `LINEUP_SERVER`, or pass `--server`. Every command takes `--json` for scripts and agents, and exit codes are stable (0 ok, 2 usage or invalid input, 3 not found, 4 conflict, 5 server unreachable, 6 a gateway failed).

Shell completions:

```sh
lineup completions zsh > ~/.zfunc/_lineup     # also bash, fish, elvish, powershell
lineup usage                                  # a usage spec (https://usage.jdx.dev)
```

CLI binaries for macOS and Linux (x86_64 and arm64) are attached to each GitHub release.

## Bring in an existing playlist

If you already have an M3U file, import it. Every entry is validated with the route you give; entries that route does not accept are skipped and listed, so a file with several kinds of channels is imported once per route. Preview first; nothing is written until the command without `--dry-run`.

```sh
lineup import ./channels.m3u -p tv -r Acestream --dry-run
lineup import ./channels.m3u -p tv -r Acestream
lineup import ./channels.m3u -p tv -r VK
lineup import ./channels.m3u -p tv -r Direct --dry-run   # check what is left before importing the rest as direct
```

Gateway rules recognise their inputs in their usual forms (for Acestream: acexy or engine URLs, also behind a path prefix, `acestream://` ids). If the gateway fails during an import, nothing is written. Re-importing is safe: channels that already exist on the route are reused and their details are kept, and the report shows where the file differs (including a different source for the same channel).

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

Add it wherever your player takes an XMLTV guide; in Jellyfin that is **Dashboard → Live TV → TV Guide Data Providers → Add → XMLTV**. A channel gets guide data when its tvg-id matches a channel id in one of the sources. A channel that no source covers gets its own name as hourly programmes (on whole hours of the server's time zone, set with `TZ`; restart to change it), so it still shows up in the guide. These are placeholders, not broadcast times.

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

## Upgrading from 0.4

lineup 0.5 starts from a new database and cannot open a 0.4 one: it refuses to start and leaves the old file untouched. Integrations are replaced by routes, and Acestream and VK support moved out of lineup into a gateway. To move over:

1. Before upgrading, while 0.4 is still running, save what you will need (into an empty folder):

   ```sh
   lineup --json ls > channels.json                 # every channel, also disabled ones and ones in no playlist
   lineup --json playlist ls > playlists.json
   for p in $(jq -r '.[].slug' playlists.json); do
     curl -fsS -o "$p.m3u" "http://media-host:9300/p/$p.m3u"
     lineup --json playlist show "$p" > "playlist-$p.json"
   done
   lineup --json epg sources ls > epg-sources.json
   lineup --json hook ls > hooks.json               # header secrets are masked: take the Jellyfin API key from Jellyfin
   lineup --json integration ls > integrations.json
   ```

2. Run a gateway that has the rules you need (for example lineup-gateway with its `acestream` and `vk` rules), configured with the engine and acexy addresses from `integrations.json`.
3. Stop and remove the 0.4 container, and keep its volume as the backup, with the 0.4 image tag in case you roll back.
4. Start 0.5 on a new, empty volume (change the volume's `name:` in the compose file, for example to `lineup-data-0.5`, and the image tag).
5. Add the gateway routes (above), then import each playlist once per route, previewing first. The gateway recognises its own and acexy's stream URLs; old VK proxy URLs (`/vk/<owner>_<video>`) are recognised if the gateway's VK rule accepts them, otherwise add those channels again from their vk.com links.
6. From the JSON records, add back the channels in no playlist and disable the ones that were disabled. Event flags and kickoff times no longer exist in 0.5. Then add the guide sources and hooks again, and set the check schedule under Settings if you used one.

Imported channels take their name, group, tvg-id and logo from the M3U.

## Health checks

lineup checks channels through their route's gateway, when the gateway's rule supports checks; how a check is done is up to the gateway. For Acestream, lineup-gateway starts a short engine session, reads its statistics and stops it again, and first asks acexy whether anyone is watching, skipping the channel if so. A check may also report what the source is now: live, ended with the recording not ready yet, or a recording.

Clicking a channel opens its details: what the gateway observes now (for Acestream, while someone watches through acexy: peers, speeds and players; opening it never starts a stream), plus the latest check and **Check now**. `lineup info <channel>` prints the same.

```sh
lineup check "Sports One"   # one channel, waits for the result
lineup check                # all of them, in the background
lineup ls --health dead
```

Scheduled checks are off by default; set **Settings → Channel checks** (or `lineup settings set check_interval_hours 6`) to turn them on. A gateway that is down gets an "unknown" result, never "dead". Hooks can subscribe to `channel.unhealthy` and `channel.recovered`.

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

A backup restores into the same lineup version line: 0.5 refuses a 0.4 database (see Upgrading from 0.4).

Re-importing an exported M3U is **not** a full restore. It brings back that playlist's channels and their M3U fields, but not disabled channels, other playlists, routes or settings. Use the database backup for that.

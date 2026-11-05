# Deploying lineup

## Run it

lineup is published as a container image, `ghcr.io/alexey-lapin/lineup`, for amd64 and arm64. The example [compose file](../deploy/compose.yaml) runs it on port 9300 and keeps the database in the named volume `lineup-data`:

```sh
docker compose -f deploy/compose.yaml up -d
```

Pin a release tag rather than `latest`. The container runs as uid 10001 and stores everything under `/data`.

Open `http://<host>:9300` and set the base URLs under **Settings**. Use addresses that Jellyfin and your players can reach, not `localhost`:

| Setting | What it is | Example |
|---|---|---|
| `acexy_base` | acexy, which plays Acestream ids | `http://media-host:8080` |
| `engine_base` | the Acestream engine, used for name lookups and (later) health checks | `http://media-host:6878` |
| `vk_base` | a VK proxy that serves `/vk/{owner}_{video}[?list=…]` | `http://media-host:8099` |

## Point the CLI at it

```sh
mkdir -p ~/.config/lineup
echo 'server = "http://media-host:9300"' > ~/.config/lineup/config.toml
lineup status
```

Or set `LINEUP_SERVER`, or pass `--server`.

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

`lineup playlist url jellyfin` prints it. The channel number in Jellyfin is the position in the playlist, so reordering in lineup reorders the guide. Until lineup serves the guide itself (see the roadmap in the spec), keep your existing XMLTV provider; the tvg-ids you set in lineup are what it matches on.

If you replace an existing tuner, Jellyfin may treat the channels as new, because channel ids depend partly on the tuner URL. Favourites can reset.

After changing channels, run Jellyfin's **Refresh Guide** task. Automatic refresh arrives with hooks.

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

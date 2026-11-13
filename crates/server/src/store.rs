//! The data store and its single mutation path.
//!
//! Every write goes through a method here. Each one runs in a transaction, collects the playlist
//! slugs whose output it affects (before anything is deleted), commits, and then calls
//! `outputs_changed_locked`, which broadcasts a change event. Writes are serialised by `write_lock`
//! so events are emitted in the same order as the states they describe.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use api_types::{
    Channel, FieldDiff, ImportAction, ImportEntry, ImportReport, Playlist, PlaylistCreate, PlaylistDetail,
    PlaylistItem, PlaylistPatch, Settings, SettingsPatch,
};

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqliteConnection, SqlitePool};
use tokio::sync::broadcast;

use crate::error::{Error, Result, conflict, invalid, is_unique_violation, not_found};
use crate::m3u;
use crate::routes::{Validated, channel_play_url};

/// Change notifications, consumed by SSE and hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PlaylistChanged {
        slugs: Vec<String>,
    },
    /// Guide content may have changed (`sources`: the sources whose data changed; empty = unspecified).
    EpgUpdated {
        sources: Vec<i64>,
    },
    /// A channel's conclusive health became `dead` (also the first result being dead).
    ChannelUnhealthy {
        channel_id: i64,
        name: String,
        detail: Option<serde_json::Value>,
    },
    /// A channel's conclusive health went from `dead` to `ok`.
    ChannelRecovered {
        channel_id: i64,
        name: String,
    },
    /// A check finished without a health transition (live UI updates only).
    ChannelChecked {
        channel_id: i64,
    },
    /// Settings changed (live UI updates only; `keys` are the settings whose values changed).
    SettingsChanged {
        keys: Vec<String>,
        revision: u64,
    },
    /// A route was created, changed, resynced or deleted (live UI updates only; playlist output
    /// changes are announced by `playlist.changed`).
    RouteChanged {
        route_id: i64,
        revision: u64,
    },
}

impl Event {
    /// Wire name, as used for SSE and hooks.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::PlaylistChanged { .. } => "playlist.changed",
            Event::EpgUpdated { .. } => "epg.updated",
            Event::ChannelUnhealthy { .. } => "channel.unhealthy",
            Event::ChannelRecovered { .. } => "channel.recovered",
            Event::ChannelChecked { .. } => "channel.checked",
            Event::SettingsChanged { .. } => "settings.changed",
            Event::RouteChanged { .. } => "route.changed",
        }
    }

    pub fn payload(&self) -> serde_json::Value {
        match self {
            Event::PlaylistChanged { slugs } => serde_json::json!({ "slugs": slugs }),
            Event::EpgUpdated { sources } => serde_json::json!({ "sources": sources }),
            Event::ChannelUnhealthy {
                channel_id,
                name,
                detail,
            } => serde_json::json!({ "channel_id": channel_id, "name": name, "detail": detail }),
            Event::ChannelRecovered { channel_id, name } => {
                serde_json::json!({ "channel_id": channel_id, "name": name })
            }
            Event::ChannelChecked { channel_id } => serde_json::json!({ "channel_id": channel_id }),
            Event::SettingsChanged { keys, revision } => serde_json::json!({ "keys": keys, "revision": revision }),
            Event::RouteChanged { route_id, revision } => {
                serde_json::json!({ "route_id": route_id, "revision": revision })
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ChannelFilter {
    /// Only channels whose conclusive health is this.
    pub health: Option<api_types::CheckOutcome>,
    pub group: Option<String>,
    pub q: Option<String>,
    pub playlist: Option<String>,
    pub route_id: Option<i64>,
}

pub struct Store {
    pool: SqlitePool,
    events: broadcast::Sender<Event>,
    write_lock: tokio::sync::Mutex<()>,
    /// Bumped with every event; output caches key on it.
    version: std::sync::atomic::AtomicU64,
    /// Bumped when hooks change, so the dispatcher reloads them.
    hooks_version: tokio::sync::watch::Sender<u64>,
}

const DEFAULT_EPG_REFRESH_HOURS: u32 = 6;
/// Upper bound for `epg_refresh_hours` (one week).
const MAX_EPG_REFRESH_HOURS: u32 = 24 * 7;
/// Upper bound for `check_interval_hours` (one week).
pub const MAX_CHECK_INTERVAL_HOURS: u32 = 24 * 7;

impl Store {
    /// Open (creating if needed) the database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5));
        // Check the lineage before anything changes the file: the WAL switch below rewrites the
        // header of a database that may belong to another release.
        let probe = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await?;
        let checked = check_lineage(&probe).await;
        probe.close().await;
        checked?;
        let opts = opts.journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
        Self::init(pool).await
    }

    /// In-memory database (tests).
    pub async fn open_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // One connection: each in-memory connection is its own database.
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self> {
        check_lineage(&pool).await?;
        MIGRATOR.run(&pool).await?;
        let (events, _) = broadcast::channel(64);
        let store = Store {
            pool,
            events,
            write_lock: tokio::sync::Mutex::new(()),
            version: std::sync::atomic::AtomicU64::new(0),
            hooks_version: tokio::sync::watch::Sender::new(0),
        };
        Ok(store)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    // ---------------------------------------------------------------- settings

    pub async fn settings(&self) -> Result<Settings> {
        let mut conn = self.pool.begin().await?; // read snapshot
        load_settings(&mut conn).await
    }

    pub async fn patch_settings(&self, patch: SettingsPatch) -> Result<Settings> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let old = load_settings(&mut tx).await?;
        let mut new = old.clone();
        if let Some(v) = patch.epg_refresh_hours {
            if v > MAX_EPG_REFRESH_HOURS {
                return Err(invalid(format!(
                    "epg_refresh_hours must be at most {MAX_EPG_REFRESH_HOURS}"
                )));
            }
            new.epg_refresh_hours = v;
        }
        if let Some(v) = patch.check_interval_hours {
            if v > MAX_CHECK_INTERVAL_HOURS {
                return Err(invalid(format!(
                    "check_interval_hours must be at most {MAX_CHECK_INTERVAL_HOURS}"
                )));
            }
            new.check_interval_hours = v;
        }
        let mut keys = Vec::new();
        if new.epg_refresh_hours != old.epg_refresh_hours {
            keys.push("epg_refresh_hours".to_string());
        }
        if new.check_interval_hours != old.check_interval_hours {
            keys.push("check_interval_hours".to_string());
        }
        if keys.is_empty() {
            return Ok(old);
        }
        new.revision = old.revision + 1;
        save_settings(&mut tx, &new).await?;
        tx.commit().await?;
        // For open UIs only: no setting affects output, and hooks cannot subscribe.
        let _ = self.events.send(Event::SettingsChanged {
            keys,
            revision: new.revision,
        });
        Ok(new)
    }

    // ---------------------------------------------------------------- channels

    pub async fn list_channels(&self, filter: &ChannelFilter) -> Result<Vec<Channel>> {
        let mut conn = self.pool.begin().await?; // read snapshot
        let mut channels = load_channels(&mut conn, None).await?;
        let q = filter.q.as_ref().map(|q| q.to_lowercase());
        channels.retain(|c| {
            filter
                .group
                .as_ref()
                .is_none_or(|g| c.group_title.as_deref() == Some(g.as_str()))
                && filter.playlist.as_ref().is_none_or(|p| c.playlists.contains(p))
                && filter.route_id.is_none_or(|r| c.route_id == r)
                && filter
                    .health
                    .is_none_or(|h| c.health.as_ref().and_then(|x| x.conclusive) == Some(h))
                && q.as_ref().is_none_or(|q| {
                    c.name.to_lowercase().contains(q)
                        || c.tvg_id.as_deref().is_some_and(|t| t.to_lowercase().contains(q))
                        || c.identity.to_lowercase().contains(q)
                        || c.source.to_lowercase().contains(q)
                })
        });
        Ok(channels)
    }

    pub async fn get_channel(&self, id: i64) -> Result<Channel> {
        let mut conn = self.pool.begin().await?; // read snapshot
        get_channel(&mut conn, id).await
    }

    /// A channel's source columns.
    pub async fn channel_source(&self, id: i64) -> Result<StoredSource> {
        let mut conn = self.pool.acquire().await?;
        stored_source(&mut conn, id).await
    }

    /// The channel on `route_id` with this identity, and its stored source.
    pub async fn find_duplicate(&self, route_id: i64, identity: &str) -> Result<Option<(i64, String)>> {
        let row = sqlx::query("SELECT id, source FROM channels WHERE route_id=? AND identity=?")
            .bind(route_id)
            .bind(identity)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| (r.get("id"), r.get("source"))))
    }

    /// Create a channel from a source validated against `route` (at `route.revision`).
    pub async fn create_channel(&self, req: NewChannelRequest) -> Result<Channel> {
        let _guard = self.write_lock.lock().await;
        let name = clean_name(&req.name)?;
        let mut tx = self.pool.begin().await?;
        expect_route(&mut tx, req.route_id, req.route_revision).await?;
        let id = insert_channel(
            &mut tx,
            &NewChannel {
                name,
                group_title: clean_opt(req.group_title),
                tvg_id: clean_opt(req.tvg_id),
                tvg_logo: clean_opt(req.tvg_logo),
                route_id: req.route_id,
                validated: req.validated,
                enabled: req.enabled,
            },
        )
        .await?;
        let mut affected = BTreeSet::new();
        for slug in dedup(req.playlists) {
            let pid = playlist_id(&mut tx, &slug).await?;
            let mut order = load_order(&mut tx, pid).await?;
            order.push(id);
            write_order(&mut tx, pid, &order).await?;
            touch_playlist(&mut tx, pid).await?;
            affected.insert(slug);
        }
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        self.get_channel(id).await
    }

    /// Update a channel. A `source` change carries a source validated against its route at a known
    /// route revision and the channel's `source_revision` it was based on; it is refused when either
    /// changed meanwhile.
    pub async fn patch_channel(&self, id: i64, patch: ChannelUpdate) -> Result<Channel> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_channel(&mut tx, id).await?;
        let cur_rev = source_revision(&mut tx, id).await?;
        let mut next = cur.clone();
        if let Some(n) = patch.name {
            next.name = clean_name(&n)?;
        }
        if let Some(v) = patch.group_title {
            next.group_title = clean_opt(v);
        }
        if let Some(v) = patch.tvg_id {
            next.tvg_id = clean_opt(v);
        }
        if let Some(v) = patch.tvg_logo {
            next.tvg_logo = clean_opt(v);
        }
        if let Some(v) = patch.enabled {
            next.enabled = v;
        }
        let mut source_changed = false;
        if let Some(sc) = &patch.source {
            if sc.based_on_source_revision != cur_rev {
                return Err(conflict("the channel's source changed meanwhile; try again"));
            }
            expect_route(&mut tx, sc.route_id, sc.route_revision).await?;
            let stored = stored_source(&mut tx, id).await?;
            source_changed = stored.route_id != sc.route_id
                || stored.source != sc.validated.source
                || stored.identity != sc.validated.identity
                || stored.play != sc.validated.play;
        }

        let meta_changed = next.name != cur.name
            || next.group_title != cur.group_title
            || next.tvg_id != cur.tvg_id
            || next.tvg_logo != cur.tvg_logo
            || next.enabled != cur.enabled;
        if !meta_changed && !source_changed {
            return Ok(cur);
        }

        sqlx::query(
            "UPDATE channels SET name=?, group_title=?, tvg_id=?, tvg_logo=?, enabled=?,
             updated_at=? WHERE id=?",
        )
        .bind(&next.name)
        .bind(&next.group_title)
        .bind(&next.tvg_id)
        .bind(&next.tvg_logo)
        .bind(next.enabled)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if source_changed {
            let sc = patch.source.as_ref().expect("source change");
            set_source(&mut tx, id, sc.route_id, &sc.validated).await?;
        }
        let affected: BTreeSet<String> = cur.playlists.iter().cloned().collect();
        tx.commit().await?;
        // Guide-affecting fields: the tvg-id selects upstream data, and a channel no source carries
        // gets its name as programmes.
        let after = self.get_channel(id).await?;
        let guide_changed = next.tvg_id != cur.tvg_id || next.name != cur.name;
        let in_playlists = !affected.is_empty();
        self.outputs_changed_locked(affected);
        if guide_changed && in_playlists {
            self.epg_changed_locked(vec![]);
        }
        Ok(after)
    }

    pub async fn delete_channel(&self, id: i64) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_channel(&mut tx, id).await?;
        // Capture affected playlists and compact their positions before deleting.
        let mut affected = BTreeSet::new();
        for slug in &cur.playlists {
            let pid = playlist_id(&mut tx, slug).await?;
            let order: Vec<i64> = load_order(&mut tx, pid)
                .await?
                .into_iter()
                .filter(|c| *c != id)
                .collect();
            write_order(&mut tx, pid, &order).await?;
            touch_playlist(&mut tx, pid).await?;
            affected.insert(slug.clone());
        }
        sqlx::query("DELETE FROM channels WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        Ok(())
    }

    // ---------------------------------------------------------------- playlists

    pub async fn list_playlists(&self) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            "SELECT p.id, p.slug, p.name, p.created_at, p.updated_at,
                    (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id) AS n
             FROM playlists p ORDER BY p.slug",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(playlist_from_row).collect()
    }

    pub async fn get_playlist(&self, slug: &str) -> Result<PlaylistDetail> {
        let mut conn = self.pool.begin().await?; // read snapshot
        get_playlist_detail(&mut conn, slug).await
    }

    pub async fn create_playlist(&self, req: PlaylistCreate) -> Result<PlaylistDetail> {
        let _guard = self.write_lock.lock().await;
        let slug = validate_slug(&req.slug)?;
        let name = match req.name {
            Some(n) => clean_name(&n)?,
            None => slug.clone(),
        };
        let mut tx = self.pool.begin().await?;
        insert_playlist(&mut tx, &slug, &name).await?;
        tx.commit().await?;
        self.outputs_changed_locked([slug.clone()].into());
        self.get_playlist(&slug).await
    }

    pub async fn patch_playlist(&self, slug: &str, patch: PlaylistPatch) -> Result<PlaylistDetail> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let pid = playlist_id(&mut tx, slug).await?;
        let mut affected = BTreeSet::from([slug.to_string()]);
        let mut new_slug = slug.to_string();
        if let Some(n) = patch.name {
            sqlx::query("UPDATE playlists SET name=? WHERE id=?")
                .bind(clean_name(&n)?)
                .bind(pid)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(s) = patch.slug {
            let s = validate_slug(&s)?;
            if s != slug {
                let res = sqlx::query("UPDATE playlists SET slug=? WHERE id=?")
                    .bind(&s)
                    .bind(pid)
                    .execute(&mut *tx)
                    .await;
                match res {
                    Err(e) if is_unique_violation(&e) => {
                        return Err(conflict(format!("playlist {s:?} already exists")));
                    }
                    r => {
                        r?;
                    }
                }
                affected.insert(s.clone());
                new_slug = s;
            }
        }
        touch_playlist(&mut tx, pid).await?;
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        self.get_playlist(&new_slug).await
    }

    pub async fn delete_playlist(&self, slug: &str) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let pid = playlist_id(&mut tx, slug).await?;
        sqlx::query("DELETE FROM playlists WHERE id=?")
            .bind(pid)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.outputs_changed_locked([slug.to_string()].into());
        Ok(())
    }

    /// Insert a channel at a 1-based position (appended when absent or past the end).
    pub async fn add_item(&self, slug: &str, channel_id: i64, position: Option<i64>) -> Result<PlaylistDetail> {
        self.mutate_order(slug, |order| {
            if order.contains(&channel_id) {
                return Err(conflict(format!(
                    "channel {channel_id} is already in playlist {slug:?}"
                )));
            }
            let at = clamp_index(position, order.len());
            order.insert(at, channel_id);
            Ok(())
        })
        .await
    }

    pub async fn remove_item(&self, slug: &str, channel_id: i64) -> Result<PlaylistDetail> {
        self.mutate_order(slug, |order| {
            let before = order.len();
            order.retain(|c| *c != channel_id);
            if order.len() == before {
                return Err(not_found(format!("channel {channel_id} is not in playlist {slug:?}")));
            }
            Ok(())
        })
        .await
    }

    pub async fn move_item(&self, slug: &str, channel_id: i64, position: i64) -> Result<PlaylistDetail> {
        self.mutate_order(slug, |order| {
            let Some(i) = order.iter().position(|c| *c == channel_id) else {
                return Err(not_found(format!("channel {channel_id} is not in playlist {slug:?}")));
            };
            order.remove(i);
            let at = clamp_index(Some(position), order.len());
            order.insert(at, channel_id);
            Ok(())
        })
        .await
    }

    /// Replace the whole membership and order of a playlist.
    pub async fn replace_items(&self, slug: &str, channel_ids: Vec<i64>) -> Result<PlaylistDetail> {
        let mut seen = HashSet::new();
        if let Some(d) = channel_ids.iter().find(|c| !seen.insert(**c)) {
            return Err(invalid(format!("channel {d} listed more than once")));
        }
        self.mutate_order(slug, move |order| {
            *order = channel_ids;
            Ok(())
        })
        .await
    }

    async fn mutate_order(&self, slug: &str, f: impl FnOnce(&mut Vec<i64>) -> Result<()>) -> Result<PlaylistDetail> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let pid = playlist_id(&mut tx, slug).await?;
        let before = load_order(&mut tx, pid).await?;
        let mut order = before.clone();
        f(&mut order)?;
        if order == before {
            return get_playlist_detail(&mut tx, slug).await;
        }
        for c in &order {
            channel_exists(&mut tx, *c).await?;
        }
        write_order(&mut tx, pid, &order).await?;
        touch_playlist(&mut tx, pid).await?;
        tx.commit().await?;
        self.outputs_changed_locked([slug.to_string()].into());
        self.get_playlist(slug).await
    }

    // ---------------------------------------------------------------- output

    /// Write a consistent, self-contained copy of the database (no WAL needed) to `path`.
    /// `path` must not exist.
    pub async fn backup_to(&self, path: &Path) -> Result<()> {
        let p = path.to_str().ok_or_else(|| invalid("backup path is not UTF-8"))?;
        sqlx::query("VACUUM INTO ?").bind(p).execute(&self.pool).await?;
        Ok(())
    }

    /// Render `/p/<slug>.m3u`.
    pub async fn render_m3u(&self, slug: &str) -> Result<String> {
        self.render_m3u_with(slug, None).await
    }

    /// Render `/p/<slug>.m3u` announcing `url_tvg` as the guide URL.
    pub async fn render_m3u_with(&self, slug: &str, url_tvg: Option<&str>) -> Result<String> {
        let mut conn = self.pool.begin().await?; // read snapshot
        render_m3u(&mut conn, slug, url_tvg).await
    }

    /// Changes whenever any output may have changed (every emitted event bumps it).
    pub fn output_version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Re-emit change notifications for the given playlists, or all.
    pub async fn refresh(&self, slugs: Vec<String>) -> Result<Vec<String>> {
        let _guard = self.write_lock.lock().await;
        let slugs: BTreeSet<String> = if slugs.is_empty() {
            self.list_playlists().await?.into_iter().map(|p| p.slug).collect()
        } else {
            let mut conn = self.pool.begin().await?; // read snapshot
            for s in &slugs {
                playlist_id(&mut conn, s).await?;
            }
            slugs.into_iter().collect()
        };
        self.outputs_changed_locked(slugs.clone());
        Ok(slugs.into_iter().collect())
    }

    /// Must be called with `write_lock` held, after the transaction committed.
    fn outputs_changed_locked(&self, slugs: BTreeSet<String>) {
        if !slugs.is_empty() {
            self.bump_version();
            let _ = self.events.send(Event::PlaylistChanged {
                slugs: slugs.into_iter().collect(),
            });
        }
    }

    fn bump_version(&self) {
        self.version.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    // ---------------------------------------------------------------- import

    /// Import entries validated against `route_id` (at `route_revision`) into playlist `slug`
    /// (created when missing): new identities become channels, existing ones are reused as they
    /// are; all are appended in file order. A dry run writes nothing.
    pub async fn import_validated(&self, req: ImportBatch) -> Result<ImportReport> {
        let ImportBatch {
            slug,
            dry_run,
            route_id,
            route_revision,
            entries: incoming,
            skipped_lines,
        } = req;
        let slug = validate_slug(&slug)?;
        let _guard = if dry_run {
            None
        } else {
            Some(self.write_lock.lock().await)
        };
        let mut tx = self.pool.begin().await?;
        expect_route(&mut tx, route_id, route_revision).await?;
        let pid = find_playlist_id(&mut tx, &slug).await?;
        let playlist_created = pid.is_none();
        let pid = match pid {
            Some(p) => Some(p),
            None if !dry_run => Some(insert_playlist(&mut tx, &slug, &slug).await?),
            None => None,
        };
        let mut order = match pid {
            Some(p) => load_order(&mut tx, p).await?,
            None => vec![],
        };
        let mut in_playlist: HashSet<i64> = order.iter().copied().collect();
        // Sources seen earlier in this file (key → channel id, or None in dry run for new ones).
        // Value: (channel id if known, the metadata later duplicates are diffed against).
        let mut seen: HashMap<String, (Option<i64>, Meta)> = HashMap::new();
        let mut entries = Vec::new();

        for inc in incoming {
            let k = inc.validated.identity.clone();
            let incoming_meta = Meta {
                name: inc.name.clone(),
                group_title: inc.group_title.clone(),
                tvg_id: inc.tvg_id.clone(),
                tvg_logo: inc.tvg_logo.clone(),
                source: inc.validated.source.clone(),
            };
            let mut diff = vec![];
            let mut reference = incoming_meta.clone();
            let (action, channel_id) = if let Some((prev, prev_meta)) = seen.get(&k) {
                diff = diff_fields(prev_meta, &incoming_meta);
                reference = prev_meta.clone();
                (ImportAction::SkipInPlaylist, *prev)
            } else if let Some(existing) = find_by_identity(&mut tx, route_id, &inc.validated.identity).await? {
                reference = Meta::from(&get_channel(&mut tx, existing).await?);
                diff = diff_fields(&reference, &incoming_meta);
                if in_playlist.contains(&existing) {
                    (ImportAction::SkipInPlaylist, Some(existing))
                } else {
                    if !dry_run {
                        order.push(existing);
                    }
                    in_playlist.insert(existing);
                    (ImportAction::Reuse, Some(existing))
                }
            } else if dry_run {
                (ImportAction::Create, None)
            } else {
                let id = insert_channel(
                    &mut tx,
                    &NewChannel {
                        name: inc.name.clone(),
                        group_title: inc.group_title.clone(),
                        tvg_id: inc.tvg_id.clone(),
                        tvg_logo: inc.tvg_logo.clone(),
                        route_id,
                        validated: inc.validated.clone(),
                        enabled: true,
                    },
                )
                .await?;
                order.push(id);
                in_playlist.insert(id);
                (ImportAction::Create, Some(id))
            };
            seen.entry(k).or_insert((channel_id, reference));
            entries.push(ImportEntry {
                index: inc.index,
                name: inc.name,
                group_title: inc.group_title,
                tvg_id: inc.tvg_id,
                tvg_logo: inc.tvg_logo,
                source: inc.validated.source,
                identity: inc.validated.identity,
                action,
                channel_id,
                diff,
            });
        }

        let report = ImportReport {
            dry_run,
            playlist_slug: slug.clone(),
            playlist_created,
            entries,
            skipped_lines,
        };
        if dry_run {
            tx.rollback().await?;
            return Ok(report);
        }
        let pid = pid.expect("playlist exists when not dry run");
        write_order(&mut tx, pid, &order).await?;
        touch_playlist(&mut tx, pid).await?;
        tx.commit().await?;
        self.outputs_changed_locked([slug].into());
        Ok(report)
    }
}

// -------------------------------------------------------------------- helpers

struct NewChannel {
    name: String,
    group_title: Option<String>,
    tvg_id: Option<String>,
    tvg_logo: Option<String>,
    route_id: i64,
    validated: Validated,
    enabled: bool,
}

/// A new channel whose source was validated against `route_id` at `route_revision`.
pub struct NewChannelRequest {
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub route_id: i64,
    pub route_revision: u64,
    pub validated: Validated,
    pub enabled: bool,
    pub playlists: Vec<String>,
}

/// A channel update; metadata fields as in `api_types::ChannelPatch`.
#[derive(Default)]
pub struct ChannelUpdate {
    pub name: Option<String>,
    pub group_title: Option<Option<String>>,
    pub tvg_id: Option<Option<String>>,
    pub tvg_logo: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub source: Option<SourceChange>,
}

/// A new source for a channel, validated against `route_id` at `route_revision`, computed from the
/// channel at `based_on_source_revision`.
pub struct SourceChange {
    pub route_id: i64,
    pub route_revision: u64,
    pub validated: Validated,
    pub based_on_source_revision: i64,
}

/// An import entry with its validated source.
pub struct ImportEntryIn {
    pub index: usize,
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub validated: Validated,
}

pub struct ImportBatch {
    pub slug: String,
    pub dry_run: bool,
    pub route_id: i64,
    pub route_revision: u64,
    pub entries: Vec<ImportEntryIn>,
    /// Lines and entries already rejected while parsing or validating.
    pub skipped_lines: Vec<String>,
}

/// What a channel's source columns hold.
pub struct StoredSource {
    pub route_id: i64,
    pub source: String,
    pub identity: String,
    pub play: Option<String>,
    pub play_revision: Option<String>,
    pub source_revision: i64,
    pub created_at: DateTime<Utc>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| Error::Internal(format!("bad timestamp {s:?}: {e}")))
}

fn clean_name(s: &str) -> Result<String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    Ok(s)
}

fn clean_opt(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|s| seen.insert(s.clone())).collect()
}

fn validate_slug(s: &str) -> Result<String> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !s.starts_with('-');
    if !ok {
        return Err(invalid(format!(
            "invalid slug {s:?}: use lowercase letters, digits, - and _"
        )));
    }
    Ok(s.to_string())
}

/// 1-based position → 0-based insert index, clamped to [0, len].
fn clamp_index(position: Option<i64>, len: usize) -> usize {
    match position {
        Some(p) if p >= 1 => ((p - 1) as usize).min(len),
        Some(_) => 0,
        None => len,
    }
}

/// Whether a channel is in playlist and guide output: it and its route are enabled.
pub fn eligible(c: &Channel) -> bool {
    c.enabled && c.route_enabled
}

fn effective_tvg_id(c: &Channel) -> String {
    c.tvg_id.clone().unwrap_or_else(|| format!("lineup.{}", c.id))
}

/// The metadata fields compared on import.
#[derive(Debug, Clone)]
struct Meta {
    name: String,
    group_title: Option<String>,
    tvg_id: Option<String>,
    tvg_logo: Option<String>,
    source: String,
}

impl From<&Channel> for Meta {
    fn from(c: &Channel) -> Self {
        Meta {
            name: c.name.clone(),
            group_title: c.group_title.clone(),
            tvg_id: c.tvg_id.clone(),
            tvg_logo: c.tvg_logo.clone(),
            source: c.source.clone(),
        }
    }
}

fn diff_fields(existing: &Meta, incoming: &Meta) -> Vec<FieldDiff> {
    let mut d = vec![];
    let mut cmp = |field: &str, existing: Option<String>, incoming: Option<String>| {
        if existing != incoming {
            d.push(FieldDiff {
                field: field.into(),
                existing,
                incoming,
            });
        }
    };
    cmp("name", Some(existing.name.clone()), Some(incoming.name.clone()));
    cmp(
        "group_title",
        existing.group_title.clone(),
        incoming.group_title.clone(),
    );
    cmp("tvg_id", existing.tvg_id.clone(), incoming.tvg_id.clone());
    cmp("tvg_logo", existing.tvg_logo.clone(), incoming.tvg_logo.clone());
    cmp("source", Some(existing.source.clone()), Some(incoming.source.clone()));
    d
}

async fn load_settings(conn: &mut SqliteConnection) -> Result<Settings> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(&mut *conn)
        .await?;
    let mut s = Settings {
        epg_refresh_hours: DEFAULT_EPG_REFRESH_HOURS,
        check_interval_hours: 0,
        revision: 0,
    };
    for r in rows {
        let key: String = r.try_get("key")?;
        let value: String = r.try_get("value")?;
        match key.as_str() {
            "epg_refresh_hours" => s.epg_refresh_hours = serde_json::from_str(&value)?,
            "check_interval_hours" => s.check_interval_hours = serde_json::from_str(&value)?,
            "revision" => s.revision = serde_json::from_str(&value)?,
            _ => {}
        }
    }
    Ok(s)
}

async fn save_settings(conn: &mut SqliteConnection, s: &Settings) -> Result<()> {
    let pairs = [
        ("epg_refresh_hours", serde_json::to_string(&s.epg_refresh_hours)?),
        ("check_interval_hours", serde_json::to_string(&s.check_interval_hours)?),
        ("revision", serde_json::to_string(&s.revision)?),
    ];
    for (k, v) in pairs {
        sqlx::query(
            "INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .bind(k)
        .bind(v)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn insert_channel(conn: &mut SqliteConnection, c: &NewChannel) -> Result<i64> {
    let ts = now();
    let v = &c.validated;
    let res = sqlx::query(
        "INSERT INTO channels(name, group_title, tvg_id, tvg_logo, route_id, source, identity, play, play_revision,
                              enabled, created_at, updated_at)
         VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&c.name)
    .bind(&c.group_title)
    .bind(&c.tvg_id)
    .bind(&c.tvg_logo)
    .bind(c.route_id)
    .bind(&v.source)
    .bind(&v.identity)
    .bind(&v.play)
    .bind(&v.play_revision)
    .bind(c.enabled)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *conn)
    .await;
    match res {
        Ok(r) => Ok(r.last_insert_rowid()),
        Err(e) if is_unique_violation(&e) => Err(duplicate(conn, c.route_id, &v.identity).await),
        Err(e) => Err(e.into()),
    }
}

/// The conflict error for an identity that already exists on a route.
async fn duplicate(conn: &mut SqliteConnection, route_id: i64, identity: &str) -> Error {
    match find_by_identity(conn, route_id, identity).await {
        Ok(Some(id)) => conflict(format!("channel {id} already has this source on this route")),
        Ok(None) => conflict("another channel already has this source on this route"),
        Err(e) => e,
    }
}

/// Point a channel at a new validated source (bumps `source_revision`; clears health, which
/// described the old source).
async fn set_source(conn: &mut SqliteConnection, id: i64, route_id: i64, v: &Validated) -> Result<()> {
    let res = sqlx::query(
        "UPDATE channels SET route_id=?, source=?, identity=?, play=?, play_revision=?,
         source_revision=source_revision+1, updated_at=? WHERE id=?",
    )
    .bind(route_id)
    .bind(&v.source)
    .bind(&v.identity)
    .bind(&v.play)
    .bind(&v.play_revision)
    .bind(now())
    .bind(id)
    .execute(&mut *conn)
    .await;
    if let Err(e) = res {
        if is_unique_violation(&e) {
            return Err(duplicate(conn, route_id, &v.identity).await);
        }
        return Err(e.into());
    }
    sqlx::query("DELETE FROM channel_health WHERE channel_id=?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn find_by_identity(conn: &mut SqliteConnection, route_id: i64, identity: &str) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT id FROM channels WHERE route_id=? AND identity=?")
        .bind(route_id)
        .bind(identity)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|r| r.get::<i64, _>("id")))
}

async fn source_revision(conn: &mut SqliteConnection, id: i64) -> Result<i64> {
    sqlx::query_scalar("SELECT source_revision FROM channels WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| not_found(format!("channel {id} not found")))
}

pub(crate) async fn stored_source(conn: &mut SqliteConnection, id: i64) -> Result<StoredSource> {
    let r = sqlx::query(
        "SELECT route_id, source, identity, play, play_revision, source_revision, created_at FROM channels WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| not_found(format!("channel {id} not found")))?;
    Ok(StoredSource {
        route_id: r.try_get("route_id")?,
        source: r.try_get("source")?,
        identity: r.try_get("identity")?,
        play: r.try_get("play")?,
        play_revision: r.try_get("play_revision")?,
        source_revision: r.try_get("source_revision")?,
        created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
    })
}

/// Fail unless route `id` exists at `revision` (it may have changed since a source was validated).
async fn expect_route(conn: &mut SqliteConnection, id: i64, revision: u64) -> Result<()> {
    let rev: Option<i64> = sqlx::query_scalar("SELECT revision FROM routes WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    match rev {
        None => Err(not_found(format!("route {id} not found"))),
        Some(r) if r as u64 != revision => Err(conflict(format!(
            "route {id} changed while the source was being validated; try again"
        ))),
        Some(_) => Ok(()),
    }
}

async fn channel_exists(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    let row = sqlx::query("SELECT 1 FROM channels WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    row.map(|_| ())
        .ok_or_else(|| not_found(format!("channel {id} not found")))
}

async fn load_channels(conn: &mut SqliteConnection, only: Option<i64>) -> Result<Vec<Channel>> {
    let routes = routes::load_all(conn).await?;
    let rows = match only {
        Some(id) => {
            sqlx::query("SELECT * FROM channels WHERE id=?")
                .bind(id)
                .fetch_all(&mut *conn)
                .await?
        }
        None => {
            sqlx::query("SELECT * FROM channels ORDER BY name COLLATE NOCASE, id")
                .fetch_all(&mut *conn)
                .await?
        }
    };
    let memberships = sqlx::query(
        "SELECT i.channel_id, p.slug FROM playlist_items i JOIN playlists p ON p.id = i.playlist_id ORDER BY p.slug",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut by_channel: HashMap<i64, Vec<String>> = HashMap::new();
    for m in memberships {
        by_channel.entry(m.get("channel_id")).or_default().push(m.get("slug"));
    }
    let mut health = health::load_health(conn, only).await?;
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            let route_id: i64 = r.try_get("route_id")?;
            let route = routes
                .get(&route_id)
                .ok_or_else(|| Error::Internal(format!("channel {id} has a missing route {route_id}")))?;
            let source: String = r.try_get("source")?;
            let play: Option<String> = r.try_get("play")?;
            Ok(Channel {
                id,
                name: r.try_get("name")?,
                group_title: r.try_get("group_title")?,
                tvg_id: r.try_get("tvg_id")?,
                tvg_logo: r.try_get("tvg_logo")?,
                route_id,
                route_name: route.name.clone(),
                route_enabled: route.enabled,
                play_url: channel_play_url(&route.config, &source, play.as_deref()),
                source,
                identity: r.try_get("identity")?,
                enabled: r.try_get("enabled")?,
                playlists: by_channel.remove(&id).unwrap_or_default(),
                health: health.remove(&id),
                created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
                updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
            })
        })
        .collect()
}

async fn get_channel(conn: &mut SqliteConnection, id: i64) -> Result<Channel> {
    load_channels(conn, Some(id))
        .await?
        .pop()
        .ok_or_else(|| not_found(format!("channel {id} not found")))
}

async fn find_playlist_id(conn: &mut SqliteConnection, slug: &str) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT id FROM playlists WHERE slug=?")
        .bind(slug)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|r| r.get("id")))
}

async fn playlist_id(conn: &mut SqliteConnection, slug: &str) -> Result<i64> {
    find_playlist_id(conn, slug)
        .await?
        .ok_or_else(|| not_found(format!("playlist {slug:?} not found")))
}

async fn insert_playlist(conn: &mut SqliteConnection, slug: &str, name: &str) -> Result<i64> {
    let ts = now();
    let res = sqlx::query("INSERT INTO playlists(slug, name, created_at, updated_at) VALUES(?, ?, ?, ?)")
        .bind(slug)
        .bind(name)
        .bind(&ts)
        .bind(&ts)
        .execute(&mut *conn)
        .await;
    match res {
        Ok(r) => Ok(r.last_insert_rowid()),
        Err(e) if is_unique_violation(&e) => Err(conflict(format!("playlist {slug:?} already exists"))),
        Err(e) => Err(e.into()),
    }
}

async fn touch_playlist(conn: &mut SqliteConnection, pid: i64) -> Result<()> {
    sqlx::query("UPDATE playlists SET updated_at=? WHERE id=?")
        .bind(now())
        .bind(pid)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn load_order(conn: &mut SqliteConnection, pid: i64) -> Result<Vec<i64>> {
    let rows = sqlx::query("SELECT channel_id FROM playlist_items WHERE playlist_id=? ORDER BY position")
        .bind(pid)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(|r| r.get("channel_id")).collect())
}

/// Make the playlist's membership exactly `order`, with dense positions 1..n.
///
/// Existing rows are first moved to negative positions so the UNIQUE(playlist_id, position)
/// constraint never trips on intermediate states.
async fn write_order(conn: &mut SqliteConnection, pid: i64, order: &[i64]) -> Result<()> {
    let current: HashSet<i64> = load_order(conn, pid).await?.into_iter().collect();
    let wanted: HashSet<i64> = order.iter().copied().collect();
    for gone in current.difference(&wanted) {
        sqlx::query("DELETE FROM playlist_items WHERE playlist_id=? AND channel_id=?")
            .bind(pid)
            .bind(gone)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("UPDATE playlist_items SET position = -position WHERE playlist_id=?")
        .bind(pid)
        .execute(&mut *conn)
        .await?;
    for (i, cid) in order.iter().enumerate() {
        let pos = i as i64 + 1;
        if current.contains(cid) {
            sqlx::query("UPDATE playlist_items SET position=? WHERE playlist_id=? AND channel_id=?")
                .bind(pos)
                .bind(pid)
                .bind(cid)
                .execute(&mut *conn)
                .await?;
        } else {
            sqlx::query("INSERT INTO playlist_items(playlist_id, channel_id, position) VALUES(?, ?, ?)")
                .bind(pid)
                .bind(cid)
                .bind(pos)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

async fn slugs_with_route(conn: &mut SqliteConnection, route_id: i64) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT DISTINCT p.slug FROM playlists p
         JOIN playlist_items i ON i.playlist_id = p.id
         JOIN channels c ON c.id = i.channel_id
         WHERE c.route_id = ?",
    )
    .bind(route_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().map(|r| r.get("slug")).collect())
}

fn playlist_from_row(r: &sqlx::sqlite::SqliteRow) -> Result<Playlist> {
    Ok(Playlist {
        id: r.try_get("id")?,
        slug: r.try_get("slug")?,
        name: r.try_get("name")?,
        channel_count: r.try_get("n")?,
        created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
        updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
    })
}

async fn get_playlist_detail(conn: &mut SqliteConnection, slug: &str) -> Result<PlaylistDetail> {
    let row = sqlx::query(
        "SELECT p.id, p.slug, p.name, p.created_at, p.updated_at,
                (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id) AS n
         FROM playlists p WHERE p.slug=?",
    )
    .bind(slug)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| not_found(format!("playlist {slug:?} not found")))?;
    let playlist = playlist_from_row(&row)?;
    let positions =
        sqlx::query("SELECT channel_id, position FROM playlist_items WHERE playlist_id=? ORDER BY position")
            .bind(playlist.id)
            .fetch_all(&mut *conn)
            .await?;
    let mut channels: HashMap<i64, Channel> = load_channels(conn, None)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    let items = positions
        .iter()
        .filter_map(|r| {
            let cid: i64 = r.get("channel_id");
            channels.remove(&cid).map(|channel| PlaylistItem {
                position: r.get("position"),
                channel,
            })
        })
        .collect();
    Ok(PlaylistDetail { playlist, items })
}

async fn render_m3u(conn: &mut SqliteConnection, slug: &str, url_tvg: Option<&str>) -> Result<String> {
    let detail = get_playlist_detail(conn, slug).await?;
    let ids: Vec<String> = detail.items.iter().map(|i| effective_tvg_id(&i.channel)).collect();
    let items: Vec<m3u::RenderItem<'_>> = detail
        .items
        .iter()
        .zip(&ids)
        .filter_map(|(i, id)| eligible(&i.channel).then_some((i, id, i.channel.play_url.as_str())))
        .map(|(i, id, url)| m3u::RenderItem {
            tvg_id: id,
            chno: i.position,
            logo: i.channel.tvg_logo.as_deref(),
            group: i.channel.group_title.as_deref(),
            name: &i.channel.name,
            url,
        })
        .collect();
    Ok(m3u::render(&items, url_tvg))
}

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

const OLD_DATABASE: &str = "this database is from lineup 0.4 or older, which this version cannot open. \
    Move the data directory aside (keep it as a backup; only the release that wrote it can read it), \
    start with an empty one and set lineup up again (see the 0.5.0 release notes)";

/// Refuse databases of another schema lineage before migrating. The lineage is identified by the
/// checksum of migration 1: databases of lineup 0.4 and older applied a different 0001. Later
/// migrations of this lineage, and newer-than-binary or dirty databases, are left to the migrator.
async fn check_lineage(pool: &SqlitePool) -> Result<()> {
    let ledger: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations')")
            .fetch_one(pool)
            .await?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'
         AND name <> '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await?;
    let first: Option<Vec<u8>> = if ledger {
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_optional(pool)
            .await?
    } else {
        None
    };
    let expected = MIGRATOR
        .iter()
        .find(|m| m.version == 1)
        .map(|m| m.checksum.as_ref())
        .ok_or_else(|| Error::Internal("migration 1 is missing".into()))?;
    match first {
        Some(c) if c == expected => Ok(()),
        Some(_) => Err(Error::UnsupportedDatabase(OLD_DATABASE.into())),
        // Empty (new) database.
        None if tables == 0 => Ok(()),
        None => Err(Error::UnsupportedDatabase(
            "the database has tables but no lineup schema history; it is not a lineup 0.5 database. \
             Point lineup at an empty data directory"
                .into(),
        )),
    }
}

mod epg;
mod health;
mod hooks;
pub(crate) mod routes;
pub use epg::{FetchOutcome, IndexedChannel, PatchedSource};
pub use health::{CheckIdentity, CheckRecord};
pub use hooks::{DELIVERIES_KEPT, NewDelivery};
pub use routes::{ChannelSource, ResyncPublish, ResyncRow, RouteRow, check_base_change, new_report};

#[cfg(test)]
mod tests;

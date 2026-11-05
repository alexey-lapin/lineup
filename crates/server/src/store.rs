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
    Channel, ChannelCreate, ChannelPatch, FieldDiff, ImportAction, ImportEntry, ImportReport, Playlist, PlaylistCreate,
    PlaylistDetail, PlaylistItem, PlaylistPatch, ResolveResponse, Settings, SettingsPatch, Source, SourceKind,
};
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqliteConnection, SqlitePool};
use tokio::sync::broadcast;

use crate::error::{Error, Result, conflict, invalid, is_unique_violation, not_found};
use crate::{m3u, source};

/// Change notifications (consumed by SSE and, later, hooks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PlaylistChanged { slugs: Vec<String> },
}

#[derive(Debug, Default, Clone)]
pub struct ChannelFilter {
    pub group: Option<String>,
    pub q: Option<String>,
    pub playlist: Option<String>,
    pub kind: Option<SourceKind>,
}

pub struct Store {
    pool: SqlitePool,
    events: broadcast::Sender<Event>,
    write_lock: tokio::sync::Mutex<()>,
}

const DEFAULT_ACEXY: &str = "http://127.0.0.1:8080";
const DEFAULT_ENGINE: &str = "http://127.0.0.1:6878";
const DEFAULT_VK: &str = "http://127.0.0.1:8099";

impl Store {
    /// Open (creating if needed) the database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
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
        sqlx::migrate!("./migrations").run(&pool).await?;
        let (events, _) = broadcast::channel(64);
        let store = Store {
            pool,
            events,
            write_lock: tokio::sync::Mutex::new(()),
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
        if let Some(v) = patch.acexy_base {
            new.acexy_base = source::normalize_base("acexy_base", &v)?;
        }
        if let Some(v) = patch.engine_base {
            new.engine_base = source::normalize_base("engine_base", &v)?;
        }
        if let Some(v) = patch.vk_base {
            new.vk_base = source::normalize_base("vk_base", &v)?;
        }
        save_settings(&mut tx, &new).await?;

        let mut affected = BTreeSet::new();
        if new.acexy_base != old.acexy_base {
            affected.extend(slugs_with_kind(&mut tx, SourceKind::Acestream).await?);
        }
        if new.vk_base != old.vk_base {
            affected.extend(slugs_with_kind(&mut tx, SourceKind::Vk).await?);
        }
        tx.commit().await?;

        self.outputs_changed_locked(affected);
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
                && filter.kind.is_none_or(|k| c.source.kind() == k)
                && q.as_ref().is_none_or(|q| {
                    c.name.to_lowercase().contains(q)
                        || c.tvg_id.as_deref().is_some_and(|t| t.to_lowercase().contains(q))
                        || c.source_key.contains(q)
                })
        });
        Ok(channels)
    }

    pub async fn get_channel(&self, id: i64) -> Result<Channel> {
        let mut conn = self.pool.begin().await?; // read snapshot
        get_channel(&mut conn, id).await
    }

    /// Parse free-form input and report the stream URL and any duplicate.
    pub async fn resolve(&self, input: &str) -> Result<ResolveResponse> {
        let src = source::parse_input(input)?;
        let key = source::source_key(&src);
        let mut conn = self.pool.begin().await?; // read snapshot
        let settings = load_settings(&mut conn).await?;
        let duplicate_of = find_by_source(&mut conn, src.kind(), &key).await?;
        Ok(ResolveResponse {
            stream_url: source::stream_url(&src, &settings),
            source: src,
            source_key: key,
            duplicate_of,
        })
    }

    pub async fn create_channel(&self, req: ChannelCreate) -> Result<Channel> {
        let _guard = self.write_lock.lock().await;
        let src = match (req.source, req.input.as_deref()) {
            (Some(s), None) => source::normalize(s)?,
            (None, Some(i)) => source::parse_input(i)?,
            _ => return Err(invalid("exactly one of `source` or `input` is required")),
        };
        let name = clean_name(&req.name)?;
        let mut tx = self.pool.begin().await?;
        let id = insert_channel(
            &mut tx,
            &NewChannel {
                name,
                group_title: clean_opt(req.group_title),
                tvg_id: clean_opt(req.tvg_id),
                tvg_logo: clean_opt(req.tvg_logo),
                source: src,
                is_event: req.is_event,
                kickoff_at: req.kickoff_at,
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

    pub async fn patch_channel(&self, id: i64, patch: ChannelPatch) -> Result<Channel> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_channel(&mut tx, id).await?;
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
        match (patch.source, patch.input.as_deref()) {
            (Some(_), Some(_)) => return Err(invalid("pass at most one of `source` or `input`")),
            (Some(s), None) => next.source = source::normalize(s)?,
            (None, Some(i)) => next.source = source::parse_input(i)?,
            (None, None) => {}
        }
        if let Some(v) = patch.is_event {
            next.is_event = v;
        }
        if let Some(v) = patch.kickoff_at {
            next.kickoff_at = v;
        }
        if let Some(v) = patch.enabled {
            next.enabled = v;
        }

        let changed = next.name != cur.name
            || next.group_title != cur.group_title
            || next.tvg_id != cur.tvg_id
            || next.tvg_logo != cur.tvg_logo
            || next.source != cur.source
            || next.is_event != cur.is_event
            || next.kickoff_at != cur.kickoff_at
            || next.enabled != cur.enabled;
        if !changed {
            return Ok(cur);
        }

        let key = source::source_key(&next.source);
        let res = sqlx::query(
            "UPDATE channels SET name=?, group_title=?, tvg_id=?, tvg_logo=?, source_kind=?, source=?,
             source_key=?, is_event=?, kickoff_at=?, enabled=?, updated_at=? WHERE id=?",
        )
        .bind(&next.name)
        .bind(&next.group_title)
        .bind(&next.tvg_id)
        .bind(&next.tvg_logo)
        .bind(next.source.kind().as_str())
        .bind(serde_json::to_string(&next.source)?)
        .bind(&key)
        .bind(next.is_event)
        .bind(next.kickoff_at.map(|t| t.to_rfc3339()))
        .bind(next.enabled)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await;
        if let Err(e) = res {
            if is_unique_violation(&e) {
                let dup = find_by_source(&mut tx, next.source.kind(), &key).await?;
                return Err(conflict(format!(
                    "another channel already has this source (id {})",
                    dup.map_or("?".into(), |d| d.to_string())
                )));
            }
            return Err(e.into());
        }
        let affected: BTreeSet<String> = cur.playlists.iter().cloned().collect();
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        self.get_channel(id).await
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
        let mut conn = self.pool.begin().await?; // read snapshot
        render_m3u(&mut conn, slug).await
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
            let _ = self.events.send(Event::PlaylistChanged {
                slugs: slugs.into_iter().collect(),
            });
        }
    }

    // ---------------------------------------------------------------- import

    pub async fn import(&self, text: &str, slug: &str, dry_run: bool) -> Result<ImportReport> {
        let slug = validate_slug(slug)?;
        let parsed = m3u::parse(text);
        let mut skipped_lines = parsed.skipped;

        // Parse every entry into a source first; unparseable URLs are reported, not fatal.
        struct Incoming {
            index: usize,
            name: String,
            group_title: Option<String>,
            tvg_id: Option<String>,
            tvg_logo: Option<String>,
            source: Source,
            key: String,
        }
        let mut incoming = Vec::new();
        for (i, e) in parsed.entries.iter().enumerate() {
            let source = match source::parse_input(&e.url) {
                Ok(s) => s,
                Err(err) => {
                    skipped_lines.push(format!("entry {} ({}): {err}", i + 1, e.name));
                    continue;
                }
            };
            let name = if e.name.trim().is_empty() {
                e.attr("tvg-name").unwrap_or(&e.url).to_string()
            } else {
                e.name.trim().to_string()
            };
            incoming.push(Incoming {
                index: i + 1,
                name,
                group_title: e.attr("group-title").map(str::to_string),
                tvg_id: e.attr("tvg-id").map(str::to_string),
                tvg_logo: e.attr("tvg-logo").map(str::to_string),
                key: source::source_key(&source),
                source,
            });
        }

        let _guard = if dry_run {
            None
        } else {
            Some(self.write_lock.lock().await)
        };
        let mut tx = self.pool.begin().await?;
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
        let mut seen: HashMap<(SourceKind, String), (Option<i64>, Meta)> = HashMap::new();
        let mut entries = Vec::new();

        for inc in incoming {
            let k = (inc.source.kind(), inc.key.clone());
            let incoming_meta = Meta {
                name: inc.name.clone(),
                group_title: inc.group_title.clone(),
                tvg_id: inc.tvg_id.clone(),
                tvg_logo: inc.tvg_logo.clone(),
                source: inc.source.clone(),
            };
            let mut diff = vec![];
            let mut reference = incoming_meta.clone();
            let (action, channel_id) = if let Some((prev, prev_meta)) = seen.get(&k) {
                diff = diff_fields(prev_meta, &incoming_meta);
                reference = prev_meta.clone();
                (ImportAction::SkipInPlaylist, *prev)
            } else if let Some(existing) = find_by_source(&mut tx, inc.source.kind(), &inc.key).await? {
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
                        source: inc.source.clone(),
                        is_event: false,
                        kickoff_at: None,
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
                source: inc.source,
                source_key: inc.key,
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
    source: Source,
    is_event: bool,
    kickoff_at: Option<DateTime<Utc>>,
    enabled: bool,
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
    source: Source,
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
    if let (Source::Vk { list: a, .. }, Source::Vk { list: b, .. }) = (&existing.source, &incoming.source) {
        cmp("vk_list", a.clone(), b.clone());
    }
    d
}

async fn load_settings(conn: &mut SqliteConnection) -> Result<Settings> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(&mut *conn)
        .await?;
    let mut s = Settings {
        acexy_base: DEFAULT_ACEXY.into(),
        engine_base: DEFAULT_ENGINE.into(),
        vk_base: DEFAULT_VK.into(),
    };
    for r in rows {
        let key: String = r.try_get("key")?;
        let value: String = r.try_get("value")?;
        match key.as_str() {
            "acexy_base" => s.acexy_base = serde_json::from_str(&value)?,
            "engine_base" => s.engine_base = serde_json::from_str(&value)?,
            "vk_base" => s.vk_base = serde_json::from_str(&value)?,
            _ => {}
        }
    }
    Ok(s)
}

async fn save_settings(conn: &mut SqliteConnection, s: &Settings) -> Result<()> {
    let pairs = [
        ("acexy_base", serde_json::to_string(&s.acexy_base)?),
        ("engine_base", serde_json::to_string(&s.engine_base)?),
        ("vk_base", serde_json::to_string(&s.vk_base)?),
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
    let key = source::source_key(&c.source);
    let ts = now();
    let res = sqlx::query(
        "INSERT INTO channels(name, group_title, tvg_id, tvg_logo, source_kind, source, source_key,
                              is_event, kickoff_at, enabled, created_at, updated_at)
         VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&c.name)
    .bind(&c.group_title)
    .bind(&c.tvg_id)
    .bind(&c.tvg_logo)
    .bind(c.source.kind().as_str())
    .bind(serde_json::to_string(&c.source)?)
    .bind(&key)
    .bind(c.is_event)
    .bind(c.kickoff_at.map(|t| t.to_rfc3339()))
    .bind(c.enabled)
    .bind(&ts)
    .bind(&ts)
    .execute(&mut *conn)
    .await;
    match res {
        Ok(r) => Ok(r.last_insert_rowid()),
        Err(e) if is_unique_violation(&e) => {
            let dup = find_by_source(conn, c.source.kind(), &key).await?;
            Err(conflict(format!(
                "a channel with this source already exists (id {})",
                dup.map_or("?".into(), |d| d.to_string())
            )))
        }
        Err(e) => Err(e.into()),
    }
}

async fn find_by_source(conn: &mut SqliteConnection, kind: SourceKind, key: &str) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT id FROM channels WHERE source_kind=? AND source_key=?")
        .bind(kind.as_str())
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.map(|r| r.get::<i64, _>("id")))
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
    let settings = load_settings(conn).await?;
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
    rows.iter()
        .map(|r| {
            let id: i64 = r.try_get("id")?;
            let source: Source = serde_json::from_str(&r.try_get::<String, _>("source")?)?;
            let kickoff: Option<String> = r.try_get("kickoff_at")?;
            Ok(Channel {
                id,
                name: r.try_get("name")?,
                group_title: r.try_get("group_title")?,
                tvg_id: r.try_get("tvg_id")?,
                tvg_logo: r.try_get("tvg_logo")?,
                stream_url: source::stream_url(&source, &settings),
                source,
                source_key: r.try_get("source_key")?,
                is_event: r.try_get("is_event")?,
                kickoff_at: kickoff.as_deref().map(parse_ts).transpose()?,
                enabled: r.try_get("enabled")?,
                playlists: by_channel.remove(&id).unwrap_or_default(),
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

async fn slugs_with_kind(conn: &mut SqliteConnection, kind: SourceKind) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT DISTINCT p.slug FROM playlists p
         JOIN playlist_items i ON i.playlist_id = p.id
         JOIN channels c ON c.id = i.channel_id
         WHERE c.source_kind = ?",
    )
    .bind(kind.as_str())
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

async fn render_m3u(conn: &mut SqliteConnection, slug: &str) -> Result<String> {
    let detail = get_playlist_detail(conn, slug).await?;
    let ids: Vec<String> = detail.items.iter().map(|i| effective_tvg_id(&i.channel)).collect();
    let items: Vec<m3u::RenderItem<'_>> = detail
        .items
        .iter()
        .zip(&ids)
        .filter(|(i, _)| i.channel.enabled)
        .map(|(i, id)| m3u::RenderItem {
            tvg_id: id,
            chno: i.position,
            logo: i.channel.tvg_logo.as_deref(),
            group: i.channel.group_title.as_deref(),
            name: &i.channel.name,
            url: &i.channel.stream_url,
        })
        .collect();
    Ok(m3u::render(&items))
}

#[cfg(test)]
mod tests;

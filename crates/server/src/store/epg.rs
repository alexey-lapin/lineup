//! EPG source rows, generation publishing and the channel index (`epg_channels`).
//!
//! Files on disk are handled by `crate::epg`; this module only touches the database. Writes take
//! `write_lock` like every other mutation so events are emitted in commit order.

use api_types::{EpgChannel, EpgSource, EpgSourceCreate, EpgSourcePatch};
use sqlx::{Row, SqliteConnection};

use super::{Event, Store, now, parse_ts};
use crate::error::{Result, invalid, not_found};

/// A channel of a generation, as written to `epg_channels` on publish.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexedChannel {
    pub xmltv_id: String,
    pub display_names: Vec<String>,
    pub icon: Option<String>,
}

/// Outcome of a fetch attempt, recorded on the source row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    Ok,
    NotModified,
    Error,
}

impl FetchOutcome {
    fn as_str(self) -> &'static str {
        match self {
            FetchOutcome::Ok => "ok",
            FetchOutcome::NotModified => "not_modified",
            FetchOutcome::Error => "error",
        }
    }
}

/// Result of patching a source: the new row and whether its URL changed (the caller fetches it).
pub struct PatchedSource {
    pub source: EpgSource,
    pub url_changed: bool,
}

impl Store {
    pub async fn list_epg_sources(&self) -> Result<Vec<EpgSource>> {
        let mut conn = self.pool.begin().await?; // read snapshot
        load_sources(&mut conn, None).await
    }

    pub async fn get_epg_source(&self, id: i64) -> Result<EpgSource> {
        let mut conn = self.pool.begin().await?;
        get_source(&mut conn, id).await
    }

    pub async fn create_epg_source(&self, req: EpgSourceCreate) -> Result<EpgSource> {
        let _guard = self.write_lock.lock().await;
        let url = validate_url(&req.url)?;
        let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => default_name(&url),
        };
        let ts = now();
        let id = sqlx::query(
            "INSERT INTO epg_sources(name, url, priority, enabled, created_at, updated_at) VALUES(?,?,?,?,?,?)",
        )
        .bind(&name)
        .bind(&url)
        .bind(req.priority)
        .bind(req.enabled)
        .bind(&ts)
        .bind(&ts)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        // No data yet, so nothing to announce.
        self.get_epg_source(id).await
    }

    pub async fn patch_epg_source(&self, id: i64, patch: EpgSourcePatch) -> Result<PatchedSource> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_source(&mut tx, id).await?;
        let mut next = cur.clone();
        if let Some(n) = patch.name {
            let n = n.trim();
            if n.is_empty() {
                return Err(invalid("name must not be empty"));
            }
            next.name = n.to_string();
        }
        if let Some(u) = patch.url {
            next.url = validate_url(&u)?;
        }
        if let Some(p) = patch.priority {
            next.priority = p;
        }
        if let Some(e) = patch.enabled {
            next.enabled = e;
        }
        if next == cur {
            return Ok(PatchedSource {
                source: cur,
                url_changed: false,
            });
        }
        sqlx::query("UPDATE epg_sources SET name=?, url=?, priority=?, enabled=?, updated_at=? WHERE id=?")
            .bind(&next.name)
            .bind(&next.url)
            .bind(next.priority)
            .bind(next.enabled)
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        // The served generation stays until the new URL's first publish, so a URL change alone does
        // not change guide content. Enable/disable and priority do.
        let has_data = cur.current_gen.is_some();
        if has_data && (next.enabled != cur.enabled || next.priority != cur.priority) {
            self.epg_changed_locked(vec![id]);
        }
        Ok(PatchedSource {
            source: self.get_epg_source(id).await?,
            url_changed: next.url != cur.url,
        })
    }

    pub async fn delete_epg_source(&self, id: i64) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_source(&mut tx, id).await?;
        sqlx::query("DELETE FROM epg_sources WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if cur.current_gen.is_some() && cur.enabled {
            self.epg_changed_locked(vec![id]);
        }
        Ok(())
    }

    /// Record a fetch attempt that did not publish anything.
    pub async fn record_epg_fetch(&self, id: i64, outcome: FetchOutcome, error: Option<&str>) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        sqlx::query("UPDATE epg_sources SET last_fetch_at=?, last_status=?, last_error=? WHERE id=?")
            .bind(now())
            .bind(outcome.as_str())
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Publish generation `generation` of source `id` in one transaction: it becomes current, the old
    /// current becomes previous, and the source's `epg_channels` rows are replaced. Fails with
    /// `not_found` if the source was deleted meanwhile.
    pub async fn publish_epg_generation(&self, id: i64, generation: i64, channels: &[IndexedChannel]) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_source(&mut tx, id).await?;
        let previous = cur.current_gen.filter(|g| *g != generation);
        sqlx::query(
            "UPDATE epg_sources SET current_gen=?, previous_gen=?, last_fetch_at=?, last_status='ok', last_error=NULL
             WHERE id=?",
        )
        .bind(generation)
        .bind(previous)
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        replace_channels(&mut tx, id, channels).await?;
        tx.commit().await?;
        if cur.enabled {
            self.epg_changed_locked(vec![id]);
        }
        Ok(())
    }

    /// Startup recovery: serve `generation` (a complete, previously published one) or nothing.
    pub async fn republish_epg_generation(
        &self,
        id: i64,
        generation: Option<i64>,
        channels: &[IndexedChannel],
    ) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        get_source(&mut tx, id).await?;
        sqlx::query("UPDATE epg_sources SET current_gen=?, previous_gen=NULL WHERE id=?")
            .bind(generation)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        replace_channels(&mut tx, id, channels).await?;
        tx.commit().await?;
        self.epg_changed_locked(vec![id]);
        Ok(())
    }

    /// Generations the database refers to, per source: `(id, current_gen, previous_gen)`.
    pub async fn epg_generations(&self) -> Result<Vec<(i64, Option<i64>, Option<i64>)>> {
        let rows = sqlx::query("SELECT id, current_gen, previous_gen FROM epg_sources")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("current_gen")?, r.try_get("previous_gen")?)))
            .collect()
    }

    /// Channels of the published generations of enabled sources, highest priority first.
    pub async fn epg_channels(&self) -> Result<Vec<EpgChannel>> {
        let rows = sqlx::query(
            "SELECT c.source_id, c.xmltv_id, c.display_names, c.icon
             FROM epg_channels c JOIN epg_sources s ON s.id = c.source_id
             WHERE s.enabled AND s.current_gen IS NOT NULL
             ORDER BY s.priority DESC, s.id, c.xmltv_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let names: String = r.try_get("display_names")?;
                Ok(EpgChannel {
                    source_id: r.try_get("source_id")?,
                    xmltv_id: r.try_get("xmltv_id")?,
                    display_names: serde_json::from_str(&names)?,
                    icon: r.try_get("icon")?,
                    score: None,
                })
            })
            .collect()
    }

    /// Must be called with `write_lock` held, after the transaction committed.
    pub(super) fn epg_changed_locked(&self, sources: Vec<i64>) {
        self.bump_version();
        let _ = self.events.send(Event::EpgUpdated { sources });
    }

    /// For each of `ids`, the source that serves it: the highest-priority enabled source with a
    /// published generation that has the id. Returns `id → (source_id, generation)`.
    pub async fn epg_lookup(&self, ids: &[String]) -> Result<std::collections::HashMap<String, (i64, i64)>> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        // Bound the number of SQL variables per statement.
        for chunk in ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT c.xmltv_id, s.id, s.current_gen FROM epg_channels c JOIN epg_sources s ON s.id = c.source_id
                 WHERE s.enabled AND s.current_gen IS NOT NULL AND c.xmltv_id IN ({marks})
                 ORDER BY s.priority DESC, s.id"
            );
            // Only "?" placeholders are interpolated; values are bound.
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
            for id in chunk {
                q = q.bind(id);
            }
            for r in q.fetch_all(&self.pool).await? {
                let id: String = r.try_get("xmltv_id")?;
                out.entry(id).or_insert((r.try_get("id")?, r.try_get("current_gen")?));
            }
        }
        Ok(out)
    }
}

async fn replace_channels(conn: &mut SqliteConnection, id: i64, channels: &[IndexedChannel]) -> Result<()> {
    sqlx::query("DELETE FROM epg_channels WHERE source_id=?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for c in channels {
        sqlx::query(
            "INSERT INTO epg_channels(source_id, xmltv_id, display_names, icon) VALUES(?,?,?,?)
             ON CONFLICT(source_id, xmltv_id) DO NOTHING",
        )
        .bind(id)
        .bind(&c.xmltv_id)
        .bind(serde_json::to_string(&c.display_names)?)
        .bind(&c.icon)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn get_source(conn: &mut SqliteConnection, id: i64) -> Result<EpgSource> {
    load_sources(conn, Some(id))
        .await?
        .pop()
        .ok_or_else(|| not_found(format!("EPG source {id} not found")))
}

async fn load_sources(conn: &mut SqliteConnection, only: Option<i64>) -> Result<Vec<EpgSource>> {
    let rows = sqlx::query(
        "SELECT s.*, (SELECT COUNT(*) FROM epg_channels c WHERE c.source_id = s.id) AS n
         FROM epg_sources s WHERE ? IS NULL OR s.id = ? ORDER BY s.priority DESC, s.id",
    )
    .bind(only)
    .bind(only)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .map(|r| {
            let last_fetch_at: Option<String> = r.try_get("last_fetch_at")?;
            Ok(EpgSource {
                id: r.try_get("id")?,
                name: r.try_get("name")?,
                url: r.try_get("url")?,
                priority: r.try_get("priority")?,
                enabled: r.try_get("enabled")?,
                current_gen: r.try_get("current_gen")?,
                last_fetch_at: last_fetch_at.as_deref().map(parse_ts).transpose()?,
                last_status: r.try_get("last_status")?,
                last_error: r.try_get("last_error")?,
                channel_count: r.try_get("n")?,
                created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
                updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
            })
        })
        .collect()
}

fn validate_url(s: &str) -> Result<String> {
    let s = s.trim();
    let url = url::Url::parse(s).map_err(|e| invalid(format!("invalid URL {s:?}: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(format!("EPG source must be an http(s) URL, got {s:?}")));
    }
    Ok(s.to_string())
}

fn default_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            let file = u.path_segments()?.next_back().filter(|f| !f.is_empty())?.to_string();
            Some(format!("{} {}", u.host_str()?, file))
        })
        .unwrap_or_else(|| url.to_string())
}

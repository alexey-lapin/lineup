//! Route rows. Name and enabled edits are plain updates; a gateway base URL edit and resyncs are
//! published together with every channel they revalidated (see `routes.rs` for validation). The
//! kind and a gateway's rule never change. A route used by channels can't be deleted.

use std::collections::{BTreeSet, HashMap};

use api_types::{ResyncItem, ResyncReport, Route, RouteConfig};
use chrono::{DateTime, Utc};
use sqlx::{Row, SqliteConnection};

use super::{Event, Store, now, parse_ts, slugs_with_route};
use crate::error::{Error, Result, conflict, invalid, is_unique_violation, not_found};
use crate::routes::{Validated, normalize_config, normalize_route_name};

/// A route as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRow {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub config: RouteConfig,
    pub revision: u64,
    pub synced_match_revision: Option<String>,
    pub synced_play_revision: Option<String>,
    pub last_resync: Option<ResyncReport>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One channel's source columns, for resync.
#[derive(Debug, Clone)]
pub struct ChannelSource {
    pub channel_id: i64,
    pub name: String,
    pub source: String,
    pub identity: String,
    pub play: Option<String>,
    pub play_revision: Option<String>,
    pub source_revision: i64,
}

/// A revalidated channel, to publish with a resync.
pub struct ResyncRow {
    pub channel_id: i64,
    /// The channel's `source_revision` the validation started from.
    pub based_on: i64,
    pub validated: Validated,
}

fn from_row(r: &sqlx::sqlite::SqliteRow) -> Result<RouteRow> {
    let kind: String = r.try_get("kind")?;
    let config = match kind.as_str() {
        "direct" => RouteConfig::Direct {},
        "gateway" => RouteConfig::Gateway {
            base_url: r.try_get("base_url")?,
            rule: r.try_get("rule")?,
        },
        other => return Err(Error::Internal(format!("unknown route kind {other:?}"))),
    };
    let last: Option<String> = r.try_get("last_resync")?;
    Ok(RouteRow {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        enabled: r.try_get("enabled")?,
        config,
        revision: r.try_get::<i64, _>("revision")? as u64,
        synced_match_revision: r.try_get("synced_match_revision")?,
        synced_play_revision: r.try_get("synced_play_revision")?,
        last_resync: last.as_deref().map(serde_json::from_str).transpose()?,
        created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
        updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
    })
}

pub(super) async fn load_all(conn: &mut SqliteConnection) -> Result<HashMap<i64, RouteRow>> {
    let rows = sqlx::query("SELECT * FROM routes").fetch_all(&mut *conn).await?;
    rows.iter().map(|r| from_row(r).map(|x| (x.id, x))).collect()
}

async fn load(conn: &mut SqliteConnection, id: i64) -> Result<RouteRow> {
    let row = sqlx::query("SELECT * FROM routes WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| not_found(format!("route {id} not found")))?;
    from_row(&row)
}

async fn describe(conn: &mut SqliteConnection, row: RouteRow) -> Result<Route> {
    let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE route_id=?")
        .bind(row.id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(Route {
        id: row.id,
        name: row.name,
        enabled: row.enabled,
        config: row.config,
        channel_count,
        revision: row.revision,
        synced_match_revision: row.synced_match_revision,
        synced_play_revision: row.synced_play_revision,
        last_resync: row.last_resync,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

impl Store {
    pub async fn list_routes(&self) -> Result<Vec<Route>> {
        let mut conn = self.pool.begin().await?; // read snapshot
        let rows = sqlx::query("SELECT * FROM routes ORDER BY name COLLATE NOCASE, id")
            .fetch_all(&mut *conn)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let row = from_row(&r)?;
            out.push(describe(&mut conn, row).await?);
        }
        Ok(out)
    }

    pub async fn get_route(&self, id: i64) -> Result<Route> {
        let mut conn = self.pool.begin().await?; // read snapshot
        let row = load(&mut conn, id).await?;
        describe(&mut conn, row).await
    }

    pub async fn route_row(&self, id: i64) -> Result<RouteRow> {
        let mut conn = self.pool.begin().await?;
        load(&mut conn, id).await
    }

    /// A route by id, or by exact name (case-insensitive).
    pub async fn find_route(&self, key: &str) -> Result<RouteRow> {
        let mut conn = self.pool.begin().await?;
        if let Ok(id) = key.trim().parse::<i64>()
            && let Ok(r) = load(&mut conn, id).await
        {
            return Ok(r);
        }
        let row = sqlx::query("SELECT * FROM routes WHERE name = ? COLLATE NOCASE")
            .bind(key.trim())
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| not_found(format!("route {key:?} not found")))?;
        from_row(&row)
    }

    /// Source columns of every channel of a route.
    pub async fn route_channel_sources(&self, route_id: i64) -> Result<Vec<ChannelSource>> {
        let rows = sqlx::query(
            "SELECT id, name, source, identity, play, play_revision, source_revision FROM channels
             WHERE route_id=? ORDER BY id",
        )
        .bind(route_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ChannelSource {
                    channel_id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    source: r.try_get("source")?,
                    identity: r.try_get("identity")?,
                    play: r.try_get("play")?,
                    play_revision: r.try_get("play_revision")?,
                    source_revision: r.try_get("source_revision")?,
                })
            })
            .collect()
    }

    /// Channels of a route whose play reference is not from `play_revision`.
    pub async fn route_pending(&self, route_id: i64, play_revision: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM channels WHERE route_id=? AND (play_revision IS NULL OR play_revision <> ?)",
        )
        .bind(route_id)
        .bind(play_revision)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn create_route(&self, name: &str, config: RouteConfig, enabled: bool) -> Result<Route> {
        let name = normalize_route_name(name)?;
        let config = normalize_config(config)?;
        let (base, rule) = match &config {
            RouteConfig::Direct {} => (None, None),
            RouteConfig::Gateway { base_url, rule } => (Some(base_url.clone()), Some(rule.clone())),
        };
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let ts = now();
        let res = sqlx::query(
            "INSERT INTO routes(name, kind, base_url, rule, enabled, created_at, updated_at) VALUES(?,?,?,?,?,?,?)",
        )
        .bind(&name)
        .bind(config.kind())
        .bind(base)
        .bind(rule)
        .bind(enabled)
        .bind(&ts)
        .bind(&ts)
        .execute(&mut *tx)
        .await;
        let id = match res {
            Ok(r) => r.last_insert_rowid(),
            Err(e) if is_unique_violation(&e) => return Err(conflict(format!("a route named {name:?} exists"))),
            Err(e) => return Err(e.into()),
        };
        let row = load(&mut tx, id).await?;
        let route = describe(&mut tx, row).await?;
        tx.commit().await?;
        let _ = self.events.send(Event::RouteChanged {
            route_id: id,
            revision: 0,
        });
        Ok(route)
    }

    /// Rename or enable/disable a route.
    pub async fn update_route(&self, id: i64, name: Option<String>, enabled: Option<bool>) -> Result<Route> {
        let name = name.map(|n| normalize_route_name(&n)).transpose()?;
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let old = load(&mut tx, id).await?;
        let new_name = name.unwrap_or_else(|| old.name.clone());
        let new_enabled = enabled.unwrap_or(old.enabled);
        if new_name == old.name && new_enabled == old.enabled {
            return describe(&mut tx, old).await;
        }
        let res = sqlx::query("UPDATE routes SET name=?, enabled=?, revision=revision+1, updated_at=? WHERE id=?")
            .bind(&new_name)
            .bind(new_enabled)
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await;
        match res {
            Err(e) if is_unique_violation(&e) => return Err(conflict(format!("a route named {new_name:?} exists"))),
            r => {
                r?;
            }
        }
        let affected: BTreeSet<String> = if new_enabled != old.enabled {
            slugs_with_route(&mut tx, id).await?.into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let row = load(&mut tx, id).await?;
        let revision = row.revision;
        let route = describe(&mut tx, row).await?;
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        let _ = self.events.send(Event::RouteChanged { route_id: id, revision });
        Ok(route)
    }

    pub async fn delete_route(&self, id: i64) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let old = load(&mut tx, id).await?;
        let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE route_id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if used > 0 {
            return Err(conflict(format!(
                "route {:?} is used by {used} channel(s); move or delete them first",
                old.name
            )));
        }
        sqlx::query("DELETE FROM routes WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let _ = self.events.send(Event::RouteChanged {
            route_id: id,
            revision: old.revision + 1,
        });
        Ok(())
    }

    /// Publish a resync of route `id`, validated at `expected_revision` (and, for a base URL change,
    /// against `new_base`). Rows whose channel changed since validation are skipped and reported.
    /// With `all_or_nothing` (a base URL change) any failed or skipped row refuses the whole
    /// publication. Returns the route and the final report.
    pub async fn publish_resync(&self, publish: ResyncPublish) -> Result<(Route, ResyncReport)> {
        let ResyncPublish {
            route_id,
            expected_revision,
            new_base,
            rows,
            mut report,
            all_or_nothing,
            name,
            enabled,
        } = publish;
        let name = name.map(|n| normalize_route_name(&n)).transpose()?;
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let old = load(&mut tx, route_id).await?;
        if old.revision != expected_revision {
            return Err(conflict(format!(
                "route {route_id} changed while its channels were being revalidated; try again"
            )));
        }
        // Channels that joined the route after the snapshot were never validated with this config.
        let validated: std::collections::HashSet<i64> = rows
            .iter()
            .map(|r| r.channel_id)
            .chain(report.failed.iter().map(|f| f.channel_id))
            .collect();
        let members = sqlx::query("SELECT id, name FROM channels WHERE route_id=?")
            .bind(route_id)
            .fetch_all(&mut *tx)
            .await?;
        for m in &members {
            let id: i64 = m.try_get("id")?;
            if !validated.contains(&id) {
                report.skipped.push(ResyncItem {
                    channel_id: id,
                    name: m.try_get("name")?,
                    reason: "it joined the route during the resync; resync again".into(),
                });
                report.total += 1;
            }
        }
        let mut changed_channels = 0usize;
        let mut writes = Vec::new();
        for row in rows {
            let cur = sqlx::query("SELECT name, source, identity, play, play_revision, source_revision FROM channels WHERE id=? AND route_id=?")
                .bind(row.channel_id)
                .bind(route_id)
                .fetch_optional(&mut *tx)
                .await?;
            let Some(cur) = cur else {
                continue; // deleted or moved meanwhile: nothing to update
            };
            let name: String = cur.try_get("name")?;
            if cur.try_get::<i64, _>("source_revision")? != row.based_on {
                report.skipped.push(ResyncItem {
                    channel_id: row.channel_id,
                    name,
                    reason: "the channel changed during the resync".into(),
                });
                continue;
            }
            let v = &row.validated;
            let same = cur.try_get::<String, _>("source")? == v.source
                && cur.try_get::<String, _>("identity")? == v.identity
                && cur.try_get::<Option<String>, _>("play")? == v.play;
            let same_rev = cur.try_get::<Option<String>, _>("play_revision")? == v.play_revision;
            if same && same_rev {
                report.unchanged += 1;
                continue;
            }
            writes.push((row.channel_id, name, same, row.validated));
        }
        if all_or_nothing && (!report.failed.is_empty() || !report.skipped.is_empty()) {
            return Err(conflict(resync_refusal(&report)));
        }
        for (id, name, same, v) in writes {
            if same {
                // Only the play revision moved: the reference itself is the same.
                sqlx::query("UPDATE channels SET play_revision=? WHERE id=?")
                    .bind(&v.play_revision)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                report.unchanged += 1;
                continue;
            }
            // An identity taken by another channel of the route is never merged.
            if let Some(other) = super::find_by_identity(&mut tx, route_id, &v.identity).await?
                && other != id
            {
                report.failed.push(ResyncItem {
                    channel_id: id,
                    name,
                    reason: format!("its new identity is already used by channel {other}"),
                });
                if all_or_nothing {
                    return Err(conflict(resync_refusal(&report)));
                }
                continue;
            }
            super::set_source(&mut tx, id, route_id, &v).await?;
            report.updated += 1;
            changed_channels += 1;
        }
        report.complete = report.failed.is_empty() && report.skipped.is_empty();
        let base_changed = new_base.is_some();
        if let Some(base) = &new_base {
            sqlx::query("UPDATE routes SET base_url=? WHERE id=?")
                .bind(base)
                .bind(route_id)
                .execute(&mut *tx)
                .await?;
        }
        let enabled_changed = enabled.is_some_and(|e| e != old.enabled);
        if name.is_some() || enabled.is_some() {
            let res = sqlx::query("UPDATE routes SET name=?, enabled=? WHERE id=?")
                .bind(name.as_ref().unwrap_or(&old.name))
                .bind(enabled.unwrap_or(old.enabled))
                .bind(route_id)
                .execute(&mut *tx)
                .await;
            if let Err(e) = res {
                if is_unique_violation(&e) {
                    return Err(conflict(format!("a route named {:?} exists", name.unwrap_or_default())));
                }
                return Err(e.into());
            }
        }
        if report.complete {
            sqlx::query("UPDATE routes SET synced_match_revision=?, synced_play_revision=? WHERE id=?")
                .bind(&report.match_revision)
                .bind(&report.play_revision)
                .bind(route_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE routes SET last_resync=?, revision=revision+1, updated_at=? WHERE id=?")
            .bind(serde_json::to_string(&report)?)
            .bind(now())
            .bind(route_id)
            .execute(&mut *tx)
            .await?;
        let affected: BTreeSet<String> = if base_changed || changed_channels > 0 || enabled_changed {
            slugs_with_route(&mut tx, route_id).await?.into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let row = load(&mut tx, route_id).await?;
        let revision = row.revision;
        let route = describe(&mut tx, row).await?;
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        let _ = self.events.send(Event::RouteChanged { route_id, revision });
        Ok((route, report))
    }
}

/// A validated resync, ready to publish.
pub struct ResyncPublish {
    pub route_id: i64,
    pub expected_revision: u64,
    /// The new gateway base URL (a base URL edit), already normalised.
    pub new_base: Option<String>,
    pub rows: Vec<ResyncRow>,
    /// Pre-filled with the validation failures and the revisions validated against.
    pub report: ResyncReport,
    pub all_or_nothing: bool,
    /// Metadata changes of the same request, applied in the same transaction.
    pub name: Option<String>,
    pub enabled: Option<bool>,
}

fn resync_refusal(report: &ResyncReport) -> String {
    let items: Vec<String> = report
        .failed
        .iter()
        .chain(&report.skipped)
        .take(5)
        .map(|i| format!("channel {} ({}): {}", i.channel_id, i.name, i.reason))
        .collect();
    format!(
        "not changed: {} channel(s) could not be revalidated with the new base URL: {}",
        report.failed.len() + report.skipped.len(),
        items.join("; ")
    )
}

/// An empty report to fill.
pub fn new_report(
    trigger: api_types::ResyncTrigger,
    total: usize,
    match_revision: Option<String>,
    play_revision: Option<String>,
) -> ResyncReport {
    ResyncReport {
        at: Utc::now(),
        trigger,
        match_revision,
        play_revision,
        total,
        updated: 0,
        unchanged: 0,
        failed: vec![],
        skipped: vec![],
        complete: false,
    }
}

pub fn check_base_change(config: &RouteConfig) -> Result<()> {
    match config {
        RouteConfig::Gateway { .. } => Ok(()),
        RouteConfig::Direct {} => Err(invalid("a direct route has no base URL")),
    }
}

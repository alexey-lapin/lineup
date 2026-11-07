//! Integration rows: one per kind, seeded by the schema. A config change that alters playback
//! URLs or which channels are in output emits `playlist.changed` for the playlists with channels
//! of that kind; every change emits `integration.changed` for open UIs.

use std::collections::BTreeSet;

use api_types::{Integration, IntegrationConfig, IntegrationPatch, SourceKind};
use sqlx::{Row, SqliteConnection};

use super::{Event, Store, now, parse_ts, slugs_with_kind};
use crate::error::{Error, Result, invalid};
use crate::integrations::{self, Configs};

struct Stored {
    config: IntegrationConfig,
    revision: u64,
    updated_at: String,
}

async fn load_row(conn: &mut SqliteConnection, kind: SourceKind) -> Result<Stored> {
    let row = sqlx::query("SELECT config, revision, updated_at FROM integrations WHERE kind=?")
        .bind(kind.as_str())
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::Internal(format!("integration {} is missing", kind.as_str())))?;
    let config: IntegrationConfig = serde_json::from_str(&row.try_get::<String, _>("config")?)?;
    if config.kind() != kind {
        return Err(Error::Internal(format!(
            "integration {} has a config of another kind",
            kind.as_str()
        )));
    }
    Ok(Stored {
        config,
        revision: row.try_get::<i64, _>("revision")? as u64,
        updated_at: row.try_get("updated_at")?,
    })
}

/// All integration configs, from one connection (pass a transaction for a consistent snapshot).
pub(super) async fn load_configs(conn: &mut SqliteConnection) -> Result<Configs> {
    let mut parts = Vec::new();
    for kind in SourceKind::ALL {
        parts.push(load_row(conn, kind).await?.config);
    }
    Ok(Configs::from_parts(parts))
}

async fn describe(conn: &mut SqliteConnection, kind: SourceKind) -> Result<Integration> {
    let stored = load_row(conn, kind).await?;
    let configs = load_configs(conn).await?;
    let channel_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE source_kind=?")
        .bind(kind.as_str())
        .fetch_one(&mut *conn)
        .await?;
    Ok(Integration {
        kind,
        configured: stored.config.configured(),
        checks: configs.checks(kind),
        config: stored.config,
        channel_count,
        revision: stored.revision,
        updated_at: parse_ts(&stored.updated_at)?,
    })
}

impl Store {
    /// A consistent snapshot of all integration configs.
    pub async fn integration_configs(&self) -> Result<Configs> {
        let mut conn = self.pool.begin().await?; // read snapshot
        load_configs(&mut conn).await
    }

    pub async fn list_integrations(&self) -> Result<Vec<Integration>> {
        let mut conn = self.pool.begin().await?; // read snapshot
        let mut out = Vec::new();
        for kind in SourceKind::ALL {
            out.push(describe(&mut conn, kind).await?);
        }
        Ok(out)
    }

    pub async fn get_integration(&self, kind: SourceKind) -> Result<Integration> {
        let mut conn = self.pool.begin().await?; // read snapshot
        describe(&mut conn, kind).await
    }

    pub async fn patch_integration(&self, kind: SourceKind, patch: IntegrationPatch) -> Result<Integration> {
        if patch.kind() != kind {
            return Err(invalid(format!(
                "this is the {} integration, but the change is for {}",
                kind.as_str(),
                patch.kind().as_str()
            )));
        }
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let before = load_configs(&mut tx).await?;
        let old = load_row(&mut tx, kind).await?;
        let new = integrations::apply(&old.config, patch)?;
        if new == old.config {
            return describe(&mut tx, kind).await;
        }
        let revision = old.revision + 1;
        sqlx::query("UPDATE integrations SET config=?, revision=?, updated_at=? WHERE kind=?")
            .bind(serde_json::to_string(&new)?)
            .bind(revision as i64)
            .bind(now())
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await?;
        let mut after = before.clone();
        after.set(new);
        // Same playback base: same URLs and the same channels in output.
        let mut affected = BTreeSet::new();
        if before.playback_base(kind) != after.playback_base(kind) {
            affected.extend(slugs_with_kind(&mut tx, kind).await?);
        }
        let result = describe(&mut tx, kind).await?;
        tx.commit().await?;
        self.outputs_changed_locked(affected);
        let _ = self.events.send(Event::IntegrationChanged { kind, revision });
        Ok(result)
    }
}

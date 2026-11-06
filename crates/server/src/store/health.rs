//! Channel health rows (§9): every attempt updates `last_attempt*`; only `ok`/`dead` update
//! `conclusive`, and transitions of `conclusive` emit events.

use std::collections::HashMap;

use api_types::{ChannelHealth, ProbeOutcome};
use sqlx::{Row, SqliteConnection};

use super::{Event, Store, now, parse_ts};
use crate::error::Result;

fn outcome_str(o: ProbeOutcome) -> &'static str {
    match o {
        ProbeOutcome::Ok => "ok",
        ProbeOutcome::Dead => "dead",
        ProbeOutcome::Unknown => "unknown",
        ProbeOutcome::Skipped => "skipped",
    }
}

fn parse_outcome(s: &str) -> ProbeOutcome {
    match s {
        "ok" => ProbeOutcome::Ok,
        "dead" => ProbeOutcome::Dead,
        "skipped" => ProbeOutcome::Skipped,
        _ => ProbeOutcome::Unknown,
    }
}

/// Which channel incarnation and source a probe measured.
pub struct ProbeIdentity<'a> {
    pub channel_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// The acestream content id that was probed.
    pub ace: &'a str,
}

impl Store {
    /// Record a probe attempt. Emits `channel.unhealthy` on `null|ok → dead`, `channel.recovered` on
    /// `dead → ok`, and `channel.probed` otherwise. Returns false, recording nothing, if the channel
    /// was deleted (even if its id was reused) or no longer has the probed source.
    pub async fn record_probe(
        &self,
        who: ProbeIdentity<'_>,
        outcome: ProbeOutcome,
        detail: Option<serde_json::Value>,
    ) -> Result<bool> {
        let _guard = self.write_lock.lock().await;
        let channel_id = who.channel_id;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT name, created_at, source_kind, source_key FROM channels WHERE id=?")
            .bind(channel_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else { return Ok(false) };
        let same = parse_ts(&row.try_get::<String, _>("created_at")?)? == who.created_at
            && row.try_get::<String, _>("source_kind")? == "acestream"
            && row.try_get::<String, _>("source_key")? == who.ace;
        if !same {
            return Ok(false);
        }
        let name: String = row.try_get("name")?;
        let prev: Option<String> = sqlx::query_scalar("SELECT conclusive FROM channel_health WHERE channel_id=?")
            .bind(channel_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
        let prev = prev.as_deref().map(parse_outcome);
        let ts = now();
        let detail_json = detail.as_ref().map(serde_json::to_string).transpose()?;
        let conclusive = matches!(outcome, ProbeOutcome::Ok | ProbeOutcome::Dead);
        if conclusive {
            sqlx::query(
                "INSERT INTO channel_health(channel_id, conclusive, conclusive_at, last_attempt, last_attempt_at, detail)
                 VALUES(?,?,?,?,?,?)
                 ON CONFLICT(channel_id) DO UPDATE SET conclusive=excluded.conclusive,
                   conclusive_at=excluded.conclusive_at, last_attempt=excluded.last_attempt,
                   last_attempt_at=excluded.last_attempt_at, detail=excluded.detail",
            )
            .bind(channel_id)
            .bind(outcome_str(outcome))
            .bind(&ts)
            .bind(outcome_str(outcome))
            .bind(&ts)
            .bind(&detail_json)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO channel_health(channel_id, last_attempt, last_attempt_at, detail) VALUES(?,?,?,?)
                 ON CONFLICT(channel_id) DO UPDATE SET last_attempt=excluded.last_attempt,
                   last_attempt_at=excluded.last_attempt_at, detail=excluded.detail",
            )
            .bind(channel_id)
            .bind(outcome_str(outcome))
            .bind(&ts)
            .bind(&detail_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let event = match (prev, outcome) {
            (None | Some(ProbeOutcome::Ok), ProbeOutcome::Dead) => Event::ChannelUnhealthy {
                channel_id,
                name,
                detail,
            },
            (Some(ProbeOutcome::Dead), ProbeOutcome::Ok) => Event::ChannelRecovered { channel_id, name },
            _ => Event::ChannelProbed { channel_id },
        };
        // Health never changes playlist or guide output, so the output version stays.
        let _ = self.events.send(event);
        Ok(true)
    }
}

pub(super) async fn load_health(conn: &mut SqliteConnection, only: Option<i64>) -> Result<HashMap<i64, ChannelHealth>> {
    let rows = sqlx::query("SELECT * FROM channel_health WHERE ? IS NULL OR channel_id = ?")
        .bind(only)
        .bind(only)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = HashMap::new();
    for r in rows {
        let conclusive: Option<String> = r.try_get("conclusive")?;
        let conclusive_at: Option<String> = r.try_get("conclusive_at")?;
        let detail: Option<String> = r.try_get("detail")?;
        out.insert(
            r.try_get("channel_id")?,
            ChannelHealth {
                conclusive: conclusive.as_deref().map(parse_outcome),
                conclusive_at: conclusive_at.as_deref().map(parse_ts).transpose()?,
                last_attempt: parse_outcome(&r.try_get::<String, _>("last_attempt")?),
                last_attempt_at: parse_ts(&r.try_get::<String, _>("last_attempt_at")?)?,
                detail: detail.as_deref().map(serde_json::from_str).transpose()?,
            },
        );
    }
    Ok(out)
}

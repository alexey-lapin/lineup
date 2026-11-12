//! Channel health rows: every attempt updates `last_attempt*`, `content_state`, `reason` and
//! `detail`; only `ok`/`dead` update `conclusive`, and transitions of `conclusive` emit events.

use std::collections::HashMap;

use api_types::{ChannelHealth, CheckOutcome, ContentState};
use sqlx::{Row, SqliteConnection};

use super::{Event, Store, now, parse_ts};
use crate::error::Result;

fn parse_outcome(s: &str) -> CheckOutcome {
    match s {
        "ok" => CheckOutcome::Ok,
        "dead" => CheckOutcome::Dead,
        "skipped" => CheckOutcome::Skipped,
        _ => CheckOutcome::Unknown,
    }
}

fn state_str(s: ContentState) -> &'static str {
    match s {
        ContentState::Live => "live",
        ContentState::EndedProcessing => "ended_processing",
        ContentState::Recording => "recording",
    }
}

fn parse_state(s: &str) -> Option<ContentState> {
    match s {
        "live" => Some(ContentState::Live),
        "ended_processing" => Some(ContentState::EndedProcessing),
        "recording" => Some(ContentState::Recording),
        _ => None,
    }
}

/// Which channel incarnation, source and route configuration a check measured.
#[derive(Debug, Clone, Copy)]
pub struct CheckIdentity {
    pub channel_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub source_revision: i64,
    pub route_id: i64,
    pub route_revision: u64,
}

pub struct CheckRecord {
    pub outcome: CheckOutcome,
    pub content_state: Option<ContentState>,
    pub reason: Option<String>,
    pub detail: Option<serde_json::Value>,
}

impl Store {
    /// Record a check attempt. Emits `channel.unhealthy` on `null|ok → dead`, `channel.recovered` on
    /// `dead → ok`, and `channel.checked` otherwise. Returns false, recording nothing, if the channel
    /// was deleted (even if its id was reused), its source changed, or its route changed meanwhile.
    pub async fn record_check(&self, who: CheckIdentity, rec: CheckRecord) -> Result<bool> {
        let _guard = self.write_lock.lock().await;
        let channel_id = who.channel_id;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT c.name, c.created_at, c.source_revision, c.route_id, r.revision AS route_revision
             FROM channels c JOIN routes r ON r.id = c.route_id WHERE c.id=?",
        )
        .bind(channel_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(false) };
        let same = parse_ts(&row.try_get::<String, _>("created_at")?)? == who.created_at
            && row.try_get::<i64, _>("source_revision")? == who.source_revision
            && row.try_get::<i64, _>("route_id")? == who.route_id
            && row.try_get::<i64, _>("route_revision")? as u64 == who.route_revision;
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
        let detail_json = rec.detail.as_ref().map(serde_json::to_string).transpose()?;
        let state = rec.content_state.map(state_str);
        let outcome = rec.outcome;
        if matches!(outcome, CheckOutcome::Ok | CheckOutcome::Dead) {
            sqlx::query(
                "INSERT INTO channel_health(channel_id, conclusive, conclusive_at, last_attempt, last_attempt_at,
                   content_state, reason, detail)
                 VALUES(?,?,?,?,?,?,?,?)
                 ON CONFLICT(channel_id) DO UPDATE SET conclusive=excluded.conclusive,
                   conclusive_at=excluded.conclusive_at, last_attempt=excluded.last_attempt,
                   last_attempt_at=excluded.last_attempt_at, content_state=excluded.content_state,
                   reason=excluded.reason, detail=excluded.detail",
            )
            .bind(channel_id)
            .bind(outcome.as_str())
            .bind(&ts)
            .bind(outcome.as_str())
            .bind(&ts)
            .bind(state)
            .bind(&rec.reason)
            .bind(&detail_json)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO channel_health(channel_id, last_attempt, last_attempt_at, content_state, reason, detail)
                 VALUES(?,?,?,?,?,?)
                 ON CONFLICT(channel_id) DO UPDATE SET last_attempt=excluded.last_attempt,
                   last_attempt_at=excluded.last_attempt_at, content_state=excluded.content_state,
                   reason=excluded.reason, detail=excluded.detail",
            )
            .bind(channel_id)
            .bind(outcome.as_str())
            .bind(&ts)
            .bind(state)
            .bind(&rec.reason)
            .bind(&detail_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let event = match (prev, outcome) {
            (None | Some(CheckOutcome::Ok), CheckOutcome::Dead) => Event::ChannelUnhealthy {
                channel_id,
                name,
                detail: rec.detail,
            },
            (Some(CheckOutcome::Dead), CheckOutcome::Ok) => Event::ChannelRecovered { channel_id, name },
            _ => Event::ChannelChecked { channel_id },
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
        let state: Option<String> = r.try_get("content_state")?;
        let detail: Option<String> = r.try_get("detail")?;
        out.insert(
            r.try_get("channel_id")?,
            ChannelHealth {
                conclusive: conclusive.as_deref().map(parse_outcome),
                conclusive_at: conclusive_at.as_deref().map(parse_ts).transpose()?,
                last_attempt: parse_outcome(&r.try_get::<String, _>("last_attempt")?),
                last_attempt_at: parse_ts(&r.try_get::<String, _>("last_attempt_at")?)?,
                content_state: state.as_deref().and_then(parse_state),
                reason: r.try_get("reason")?,
                detail: detail.as_deref().map(serde_json::from_str).transpose()?,
            },
        );
    }
    Ok(out)
}

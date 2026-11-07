//! Hook rows and the delivery log.

use api_types::{DeliveryTrigger, EVENT_TYPES, Hook, HookConfig, HookCreate, HookDelivery, HookPatch, SECRET_MASK};
use sqlx::{Row, SqliteConnection};

use super::{Store, now, parse_ts};
use crate::error::{Result, invalid, not_found};

/// Delivery rows kept per hook.
pub const DELIVERIES_KEPT: i64 = 200;
const DEFAULT_DEBOUNCE_SECS: u32 = 30;
const MAX_DEBOUNCE_SECS: u32 = 3600;

/// A delivery attempt to record.
pub struct NewDelivery<'a> {
    pub hook_id: i64,
    pub events: &'a [String],
    pub trigger: DeliveryTrigger,
    pub attempt: u32,
    pub ok: bool,
    pub http_code: Option<u16>,
    pub error: Option<String>,
}

impl Store {
    /// Hooks with secrets masked (for the API).
    pub async fn list_hooks(&self) -> Result<Vec<Hook>> {
        Ok(self.hooks_with_secrets().await?.into_iter().map(masked).collect())
    }

    pub async fn get_hook(&self, id: i64) -> Result<Hook> {
        let mut conn = self.pool.acquire().await?;
        get_hook(&mut conn, id).await.map(masked)
    }

    /// Hooks including their secrets (for the dispatcher).
    pub async fn hooks_with_secrets(&self) -> Result<Vec<Hook>> {
        let mut conn = self.pool.acquire().await?;
        load_hooks(&mut conn, None).await
    }

    pub async fn hook_with_secrets(&self, id: i64) -> Result<Hook> {
        let mut conn = self.pool.acquire().await?;
        get_hook(&mut conn, id).await
    }

    pub async fn create_hook(&self, req: HookCreate) -> Result<Hook> {
        let _guard = self.write_lock.lock().await;
        let name = clean_hook_name(&req.name)?;
        if contains_mask(&req.config) {
            return Err(invalid("secrets must be set to real values, not the mask"));
        }
        let config = validate_config(req.config)?;
        let events = match req.events {
            Some(e) => validate_events(e)?,
            None => default_events(&config),
        };
        let debounce = validate_debounce(req.debounce_secs.unwrap_or(DEFAULT_DEBOUNCE_SECS))?;
        let ts = now();
        let id = sqlx::query(
            "INSERT INTO hooks(name, kind, config, events, debounce_secs, enabled, created_at, updated_at)
             VALUES(?,?,?,?,?,?,?,?)",
        )
        .bind(&name)
        .bind(kind(&config))
        .bind(serde_json::to_string(&config)?)
        .bind(serde_json::to_string(&events)?)
        .bind(debounce)
        .bind(req.enabled)
        .bind(&ts)
        .bind(&ts)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        self.hooks_changed();
        self.get_hook(id).await
    }

    pub async fn patch_hook(&self, id: i64, patch: HookPatch) -> Result<Hook> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let cur = get_hook(&mut tx, id).await?;
        let mut next = cur.clone();
        if let Some(n) = patch.name {
            next.name = clean_hook_name(&n)?;
        }
        if let Some(c) = patch.config {
            let c = keep_masked_secrets(c, &cur.config);
            if contains_mask(&c) {
                return Err(invalid("secrets must be set to real values, not the mask"));
            }
            next.config = validate_config(c)?;
        }
        if let Some(e) = patch.events {
            next.events = validate_events(e)?;
        }
        if let Some(d) = patch.debounce_secs {
            next.debounce_secs = validate_debounce(d)?;
        }
        if let Some(e) = patch.enabled {
            next.enabled = e;
        }
        if next != cur {
            sqlx::query(
                "UPDATE hooks SET name=?, kind=?, config=?, events=?, debounce_secs=?, enabled=?, updated_at=?
                 WHERE id=?",
            )
            .bind(&next.name)
            .bind(kind(&next.config))
            .bind(serde_json::to_string(&next.config)?)
            .bind(serde_json::to_string(&next.events)?)
            .bind(next.debounce_secs)
            .bind(next.enabled)
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            self.hooks_changed();
        } else {
            drop(tx);
        }
        self.get_hook(id).await
    }

    pub async fn delete_hook(&self, id: i64) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let res = sqlx::query("DELETE FROM hooks WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(not_found(format!("hook {id} not found")));
        }
        self.hooks_changed();
        Ok(())
    }

    /// Record a delivery attempt and trim the hook's log; returns the recorded row, or `None` if the
    /// hook was deleted. Serialized with other writes so concurrent workers never hit a busy writer.
    pub async fn record_delivery(&self, d: NewDelivery<'_>) -> Result<Option<HookDelivery>> {
        let _guard = self.write_lock.lock().await;
        let mut tx = self.pool.begin().await?;
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM hooks WHERE id=?")
            .bind(d.hook_id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Ok(None);
        }
        let at = now();
        let id = sqlx::query(
            "INSERT INTO hook_deliveries(hook_id, events, trigger, attempt, ok, http_code, error, at)
             VALUES(?,?,?,?,?,?,?,?)",
        )
        .bind(d.hook_id)
        .bind(serde_json::to_string(d.events)?)
        .bind(trigger_str(d.trigger))
        .bind(d.attempt)
        .bind(d.ok)
        .bind(d.http_code)
        .bind(&d.error)
        .bind(&at)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        sqlx::query(
            "DELETE FROM hook_deliveries WHERE hook_id=? AND id NOT IN
             (SELECT id FROM hook_deliveries WHERE hook_id=? ORDER BY id DESC LIMIT ?)",
        )
        .bind(d.hook_id)
        .bind(d.hook_id)
        .bind(DELIVERIES_KEPT)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(HookDelivery {
            id,
            hook_id: d.hook_id,
            events: d.events.to_vec(),
            trigger: d.trigger,
            attempt: d.attempt,
            ok: d.ok,
            http_code: d.http_code,
            error: d.error,
            at: parse_ts(&at)?,
        }))
    }

    /// The most recent deliveries of a hook, newest first.
    pub async fn hook_deliveries(&self, id: i64, limit: i64) -> Result<Vec<HookDelivery>> {
        let mut conn = self.pool.acquire().await?;
        get_hook(&mut conn, id).await?;
        let rows = sqlx::query("SELECT * FROM hook_deliveries WHERE hook_id=? ORDER BY id DESC LIMIT ?")
            .bind(id)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter()
            .map(|r| {
                let events: String = r.try_get("events")?;
                let trigger: String = r.try_get("trigger")?;
                let http_code: Option<i64> = r.try_get("http_code")?;
                Ok(HookDelivery {
                    id: r.try_get("id")?,
                    hook_id: r.try_get("hook_id")?,
                    events: serde_json::from_str(&events)?,
                    trigger: match trigger.as_str() {
                        "test" => DeliveryTrigger::Test,
                        _ => DeliveryTrigger::Events,
                    },
                    attempt: r.try_get::<i64, _>("attempt")? as u32,
                    ok: r.try_get("ok")?,
                    http_code: http_code.and_then(|c| u16::try_from(c).ok()),
                    error: r.try_get("error")?,
                    at: parse_ts(&r.try_get::<String, _>("at")?)?,
                })
            })
            .collect()
    }

    /// Hook configuration changed: the dispatcher reloads.
    fn hooks_changed(&self) {
        self.hooks_version.send_modify(|v| *v += 1);
    }

    /// Changes whenever a hook is created, changed or deleted.
    pub fn watch_hooks(&self) -> tokio::sync::watch::Receiver<u64> {
        self.hooks_version.subscribe()
    }
}

fn trigger_str(t: DeliveryTrigger) -> &'static str {
    match t {
        DeliveryTrigger::Events => "events",
        DeliveryTrigger::Test => "test",
    }
}

fn kind(c: &HookConfig) -> &'static str {
    match c {
        HookConfig::Webhook { .. } => "webhook",
    }
}

fn default_events(_: &HookConfig) -> Vec<String> {
    EVENT_TYPES.iter().map(|s| s.to_string()).collect()
}

fn clean_hook_name(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    Ok(s.to_string())
}

fn validate_events(events: Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        let e = e.trim().to_string();
        if !EVENT_TYPES.contains(&e.as_str()) {
            return Err(invalid(format!(
                "unknown event type {e:?} (known: {})",
                EVENT_TYPES.join(", ")
            )));
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    if out.is_empty() {
        return Err(invalid("a hook needs at least one event type"));
    }
    Ok(out)
}

fn validate_debounce(d: u32) -> Result<u32> {
    if d > MAX_DEBOUNCE_SECS {
        return Err(invalid(format!("debounce_secs must be at most {MAX_DEBOUNCE_SECS}")));
    }
    Ok(d)
}

fn http_url(field: &str, s: &str) -> Result<String> {
    let s = s.trim();
    let u = url::Url::parse(s).map_err(|e| invalid(format!("{field}: invalid URL {s:?}: {e}")))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(invalid(format!("{field}: must be an http(s) URL")));
    }
    Ok(s.trim_end_matches('/').to_string())
}

fn validate_config(c: HookConfig) -> Result<HookConfig> {
    Ok(match c {
        HookConfig::Webhook { method, url, headers } => {
            let method = method.trim().to_ascii_uppercase();
            if !matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
                return Err(invalid("method must be POST, PUT or PATCH"));
            }
            for (k, v) in &headers {
                if reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err()
                    || reqwest::header::HeaderValue::from_str(v).is_err()
                {
                    return Err(invalid(format!("invalid header {k:?}")));
                }
            }
            // The URL is kept as given (a trailing slash can matter to a webhook receiver).
            let url = url.trim().to_string();
            http_url("url", &url)?;
            HookConfig::Webhook { method, url, headers }
        }
    })
}

fn contains_mask(c: &HookConfig) -> bool {
    match c {
        HookConfig::Webhook { headers, .. } => headers.values().any(|v| v == SECRET_MASK),
    }
}

/// Replace masks sent back by a client with the stored secrets (by header name).
fn keep_masked_secrets(mut next: HookConfig, cur: &HookConfig) -> HookConfig {
    let (HookConfig::Webhook { headers, .. }, HookConfig::Webhook { headers: old, .. }) = (&mut next, cur);
    for (k, v) in headers.iter_mut() {
        if v == SECRET_MASK
            && let Some(o) = old.get(k)
        {
            *v = o.clone();
        }
    }
    next
}

/// The hook as shown by the API: secrets replaced by the mask.
fn masked(mut h: Hook) -> Hook {
    match &mut h.config {
        HookConfig::Webhook { headers, .. } => {
            for v in headers.values_mut() {
                *v = SECRET_MASK.into();
            }
        }
    }
    h
}

async fn get_hook(conn: &mut SqliteConnection, id: i64) -> Result<Hook> {
    load_hooks(conn, Some(id))
        .await?
        .pop()
        .ok_or_else(|| not_found(format!("hook {id} not found")))
}

async fn load_hooks(conn: &mut SqliteConnection, only: Option<i64>) -> Result<Vec<Hook>> {
    let rows = sqlx::query("SELECT * FROM hooks WHERE ? IS NULL OR id = ? ORDER BY id")
        .bind(only)
        .bind(only)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter()
        .map(|r| {
            let config: String = r.try_get("config")?;
            let events: String = r.try_get("events")?;
            Ok(Hook {
                id: r.try_get("id")?,
                name: r.try_get("name")?,
                config: serde_json::from_str(&config)?,
                events: serde_json::from_str(&events)?,
                debounce_secs: r.try_get::<i64, _>("debounce_secs")? as u32,
                enabled: r.try_get("enabled")?,
                created_at: parse_ts(&r.try_get::<String, _>("created_at")?)?,
                updated_at: parse_ts(&r.try_get::<String, _>("updated_at")?)?,
            })
        })
        .collect()
}

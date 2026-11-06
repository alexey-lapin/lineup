//! Connection tests for the service base URLs in settings. Read-only requests that never start a
//! stream; a failed connection is a negative result, not an error.

use std::time::{Duration, Instant};

use api_types::{SettingsTestResult, SettingsTestTarget};
use serde_json::Value;

use crate::error::Result;
use crate::source;
use crate::store::Store;

const TIMEOUT: Duration = Duration::from_secs(5);
/// Answers are small; anything larger is cut off here.
const MAX_BODY: usize = 64 * 1024;

pub async fn test(
    store: &Store,
    client: &reqwest::Client,
    target: SettingsTestTarget,
    base: Option<&str>,
) -> Result<SettingsTestResult> {
    let (field, saved) = {
        let s = store.settings().await?;
        match target {
            SettingsTestTarget::Acexy => ("acexy_base", s.acexy_base),
            SettingsTestTarget::Engine => ("engine_base", s.engine_base),
            SettingsTestTarget::Vk => ("vk_base", s.vk_base),
        }
    };
    let base = source::normalize_base(field, base.unwrap_or(&saved))?;
    let url = match target {
        SettingsTestTarget::Acexy => format!("{base}/ace/status"),
        SettingsTestTarget::Engine => format!("{base}/webui/api/service?method=get_version"),
        SettingsTestTarget::Vk => format!("{base}/"),
    };
    let started = Instant::now();
    let mut result = SettingsTestResult {
        ok: false,
        url: url.clone(),
        detail: String::new(),
        http_status: None,
        version: None,
        elapsed_ms: 0,
    };
    match fetch(client, &url).await {
        Err(e) => result.detail = format!("no answer: {e}"),
        Ok((status, body)) => {
            result.http_status = Some(status);
            judge(target, status, &body, &mut result);
        }
    }
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(result)
}

fn judge(target: SettingsTestTarget, status: u16, body: &[u8], r: &mut SettingsTestResult) {
    let success = (200..300).contains(&status);
    let json: Option<Value> = serde_json::from_slice(body).ok();
    match target {
        SettingsTestTarget::Acexy => match (success, &json) {
            (true, Some(v)) => {
                r.ok = true;
                r.detail = match v.get("streams").and_then(Value::as_u64) {
                    Some(n) => format!("acexy answered; {n} active stream(s)"),
                    None => "acexy answered".into(),
                };
            }
            (true, None) => r.detail = format!("HTTP {status}, but not an acexy status response"),
            (false, _) => r.detail = format!("HTTP {status}"),
        },
        SettingsTestTarget::Engine => {
            let Some(v) = json.filter(|_| success) else {
                r.detail = format!("HTTP {status}, but not an Acestream engine response");
                return;
            };
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                r.detail = format!("the engine answered with an error: {}", text(err));
                return;
            }
            let result = v.get("result");
            match result.and_then(|x| x.get("version")).and_then(Value::as_str) {
                Some(version) => {
                    r.ok = true;
                    r.version = Some(version.to_string());
                    let platform = result.and_then(|x| x.get("platform")).and_then(Value::as_str);
                    r.detail = match platform {
                        Some(p) => format!("Acestream engine {version} ({p})"),
                        None => format!("Acestream engine {version}"),
                    };
                }
                None => r.detail = format!("HTTP {status}, but not an Acestream engine response"),
            }
        }
        // The proxy has no known status endpoint: any HTTP answer shows only that something listens.
        SettingsTestTarget::Vk => {
            r.ok = true;
            r.detail = format!("an HTTP service answered (status {status})");
        }
    }
}

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// GET with a deadline, reading at most `MAX_BODY` bytes.
async fn fetch(client: &reqwest::Client, url: &str) -> std::result::Result<(u16, Vec<u8>), String> {
    let work = async {
        let mut res = client.get(url).send().await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| e.to_string())? {
            let room = MAX_BODY - body.len();
            body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if body.len() >= MAX_BODY {
                break;
            }
        }
        Ok((status, body))
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err(format!("timed out after {}s", TIMEOUT.as_secs())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(target: SettingsTestTarget, status: u16, body: &str) -> SettingsTestResult {
        let mut r = SettingsTestResult {
            ok: false,
            url: String::new(),
            detail: String::new(),
            http_status: Some(status),
            version: None,
            elapsed_ms: 0,
        };
        judge(target, status, body.as_bytes(), &mut r);
        r
    }

    #[test]
    fn engine_envelope() {
        let r = run(
            SettingsTestTarget::Engine,
            200,
            r#"{"result": {"code": 3, "platform": "linux", "version": "3.2.3"}, "error": null}"#,
        );
        assert!(r.ok);
        assert_eq!(r.version.as_deref(), Some("3.2.3"));
        assert_eq!(r.detail, "Acestream engine 3.2.3 (linux)");
        let r = run(
            SettingsTestTarget::Engine,
            200,
            r#"{"result": null, "error": "unknown method"}"#,
        );
        assert!(!r.ok);
        assert!(r.detail.contains("unknown method"));
        assert!(!run(SettingsTestTarget::Engine, 200, "<html>").ok);
        assert!(!run(SettingsTestTarget::Engine, 200, r#"{"result": {}, "error": null}"#).ok);
        assert!(!run(SettingsTestTarget::Engine, 500, r#"{"result": {"version": "1"}}"#).ok);
    }

    #[test]
    fn acexy_and_vk() {
        let r = run(SettingsTestTarget::Acexy, 200, r#"{"streams": 2}"#);
        assert!(r.ok && r.detail.contains("2 active"));
        assert!(!run(SettingsTestTarget::Acexy, 200, "not json").ok);
        assert!(!run(SettingsTestTarget::Acexy, 404, "{}").ok);
        let r = run(SettingsTestTarget::Vk, 404, "");
        assert!(r.ok);
        assert_eq!(r.detail, "an HTTP service answered (status 404)");
    }
}

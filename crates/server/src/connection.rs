//! Connection tests for integration components, and integration status (the same tests on the
//! saved config). Read-only requests that never start a stream; a failed connection is a negative
//! result, not an error.

use std::time::{Duration, Instant};

use api_types::{Component, ConnectionTestResult, IntegrationStatus, SourceKind};
use serde_json::Value;

use crate::error::{Result, invalid};
use crate::integrations::Configs;
use crate::source;
use crate::store::Store;

pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Answers are small; anything larger is cut off here.
pub const MAX_BODY: usize = 64 * 1024;

/// The components of an integration kind.
pub fn components(kind: SourceKind) -> &'static [Component] {
    match kind {
        SourceKind::Acestream => &[Component::Engine, Component::Acexy],
        SourceKind::Vk => &[Component::Proxy],
        SourceKind::Direct => &[],
    }
}

/// The saved URL of a component ("" when not set).
fn saved_url(configs: &Configs, c: Component) -> String {
    match c {
        Component::Engine => configs.acestream.engine_url.clone(),
        Component::Acexy => configs.acestream.acexy_url.clone().unwrap_or_default(),
        Component::Proxy => configs.vk_url.clone(),
    }
}

/// Test a component of `kind` at `url`, or at its saved URL.
pub async fn test(
    store: &Store,
    kind: SourceKind,
    component: Component,
    url: Option<&str>,
) -> Result<ConnectionTestResult> {
    if !components(kind).contains(&component) {
        return Err(invalid(format!(
            "the {} integration has no {} to test",
            kind.as_str(),
            component.as_str()
        )));
    }
    let saved = saved_url(&store.integration_configs().await?, component);
    let raw = url.unwrap_or(&saved);
    if raw.trim().is_empty() {
        return Err(invalid(format!("no {} URL to test", component.as_str())));
    }
    let base = source::normalize_base(component.as_str(), raw)?;
    Ok(run(component, &base).await)
}

/// Test every configured component with its saved URL, concurrently.
pub async fn status(store: &Store, kind: SourceKind) -> Result<IntegrationStatus> {
    let configs = store.integration_configs().await?;
    let tests = components(kind).iter().filter_map(|c| {
        let url = saved_url(&configs, *c);
        (!url.is_empty()).then_some(async move { run(*c, &url).await })
    });
    Ok(IntegrationStatus {
        kind,
        configured: configs.configured(kind),
        components: futures_util::future::join_all(tests).await,
    })
}

async fn run(component: Component, base: &str) -> ConnectionTestResult {
    let url = match component {
        Component::Acexy => format!("{base}/ace/status"),
        Component::Engine => format!("{base}/webui/api/service?method=get_version"),
        Component::Proxy => format!("{base}/"),
    };
    let started = Instant::now();
    let mut result = ConnectionTestResult {
        component,
        ok: false,
        url: url.clone(),
        detail: String::new(),
        http_status: None,
        version: None,
        active_streams: None,
        elapsed_ms: 0,
    };
    // Body only where it is judged: the proxy check is complete once response headers arrive.
    let read_body = component != Component::Proxy;
    match fetch(&url, read_body).await {
        Err(e) => result.detail = format!("no answer: {e}"),
        Ok((status, body)) => {
            result.http_status = Some(status);
            judge(component, status, &body, &mut result);
        }
    }
    result.elapsed_ms = started.elapsed().as_millis() as u64;
    result
}

fn judge(component: Component, status: u16, body: &[u8], r: &mut ConnectionTestResult) {
    let success = (200..300).contains(&status);
    let json: Option<Value> = serde_json::from_slice(body).ok();
    match component {
        Component::Acexy => match (success, &json) {
            (true, Some(v)) => {
                r.ok = true;
                r.active_streams = v.get("streams").and_then(Value::as_u64);
                r.detail = match r.active_streams {
                    Some(n) => format!("acexy answered; {n} active stream(s)"),
                    None => "acexy answered".into(),
                };
            }
            (true, None) => r.detail = format!("HTTP {status}, but not an acexy status response"),
            (false, _) => r.detail = format!("HTTP {status}"),
        },
        Component::Engine => {
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
        Component::Proxy => {
            r.ok = true;
            r.detail = format!("an HTTP service answered (status {status})");
        }
    }
}

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// GET with a deadline, not following redirects (the answer of the address itself counts), reading
/// at most `MAX_BODY` bytes of the body when asked to. Errors do not repeat the URL.
pub async fn fetch(url: &str, read_body: bool) -> std::result::Result<(u16, Vec<u8>), String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.without_url().to_string())?;
    fetch_with(&client, url, read_body, TIMEOUT).await
}

/// As [`fetch`], with a given client (which should not follow redirects).
pub async fn fetch_with(
    client: &reqwest::Client,
    url: &str,
    read_body: bool,
    timeout: Duration,
) -> std::result::Result<(u16, Vec<u8>), String> {
    let work = async {
        let mut res = client.get(url).send().await.map_err(|e| e.without_url().to_string())?;
        let status = res.status().as_u16();
        let mut body = Vec::new();
        if !read_body {
            return Ok((status, body));
        }
        while let Some(chunk) = res.chunk().await.map_err(|e| e.without_url().to_string())? {
            let room = MAX_BODY - body.len();
            body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            if body.len() >= MAX_BODY {
                break;
            }
        }
        Ok((status, body))
    };
    tokio::time::timeout(timeout, work)
        .await
        .unwrap_or_else(|_| Err(format!("timed out after {}s", timeout.as_secs())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judged(component: Component, status: u16, body: &str) -> ConnectionTestResult {
        let mut r = ConnectionTestResult {
            component,
            ok: false,
            url: String::new(),
            detail: String::new(),
            http_status: Some(status),
            version: None,
            active_streams: None,
            elapsed_ms: 0,
        };
        judge(component, status, body.as_bytes(), &mut r);
        r
    }

    #[test]
    fn engine_envelope() {
        let r = judged(
            Component::Engine,
            200,
            r#"{"result": {"code": 3, "platform": "linux", "version": "3.2.3"}, "error": null}"#,
        );
        assert!(r.ok);
        assert_eq!(r.version.as_deref(), Some("3.2.3"));
        assert_eq!(r.detail, "Acestream engine 3.2.3 (linux)");
        let r = judged(Component::Engine, 200, r#"{"result": null, "error": "unknown method"}"#);
        assert!(!r.ok);
        assert!(r.detail.contains("unknown method"));
        assert!(!judged(Component::Engine, 200, "<html>").ok);
        assert!(!judged(Component::Engine, 200, r#"{"result": {}, "error": null}"#).ok);
        assert!(!judged(Component::Engine, 500, r#"{"result": {"version": "1"}}"#).ok);
    }

    #[test]
    fn acexy_and_proxy() {
        let r = judged(Component::Acexy, 200, r#"{"streams": 2}"#);
        assert!(r.ok && r.detail.contains("2 active"));
        assert_eq!(r.active_streams, Some(2));
        let r = judged(Component::Acexy, 200, "{}");
        assert!(r.ok);
        assert_eq!(r.active_streams, None, "unknown, not zero");
        assert!(!judged(Component::Acexy, 200, "not json").ok);
        assert!(!judged(Component::Acexy, 404, "{}").ok);
        let r = judged(Component::Proxy, 404, "");
        assert!(r.ok);
        assert_eq!(r.detail, "an HTTP service answered (status 404)");
    }
}

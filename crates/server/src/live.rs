//! Channel details read live from the services: the engine's name for a content id, and the numbers
//! of the session acexy has open for it. Nothing here starts, controls or stops a session: the
//! numbers come only from the stat URL acexy reports for a stream it is already serving, and only
//! that stat endpoint is requested. Nothing is recorded as health.

use std::sync::LazyLock;
use std::time::Duration;

use api_types::{AcestreamInfo, AcestreamLive, ChannelInfo, LiveState, Source};
use serde_json::Value;

use crate::connection;
use crate::error::Result;
use crate::store::Store;

const TIMEOUT: Duration = Duration::from_secs(4);

/// Never follows redirects: a redirect could lead to a playback or control URL.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("building an HTTP client")
});

/// Details of channel `id`; `with_name` also asks the engine for the content's name.
pub async fn channel_info(store: &Store, id: i64, with_name: bool) -> Result<ChannelInfo> {
    let channel = store.get_channel(id).await?;
    let configs = store.integration_configs().await?;
    let kind = channel.source.kind();
    let acestream = match &channel.source {
        Source::Acestream { id: ace } => {
            let a = &configs.acestream;
            let (engine_name, engine_name_error) = if !with_name {
                (None, None)
            } else if a.engine_url.is_empty() {
                (None, Some("the Acestream engine is not configured".into()))
            } else {
                match crate::resolve::engine_name(&CLIENT, &a.engine_url, ace).await {
                    Ok(Some(n)) => (Some(n), None),
                    Ok(None) => (None, Some("the engine has no name for this id".into())),
                    Err(e) => (None, Some(e)),
                }
            };
            let live = if a.engine_url.is_empty() {
                unavailable("the Acestream engine is not configured")
            } else if let Some(acexy) = &a.acexy_url {
                acestream_live(acexy, &a.engine_url, ace).await
            } else {
                unavailable("live numbers come from acexy, which is not configured")
            };
            Some(AcestreamInfo {
                engine_name,
                engine_name_error,
                live,
            })
        }
        _ => None,
    };
    Ok(ChannelInfo {
        channel_id: channel.id,
        kind,
        configured: channel.configured,
        checks: configs.checks(kind),
        health: channel.health,
        acestream,
    })
}

fn empty(state: LiveState, reason: Option<String>) -> AcestreamLive {
    AcestreamLive {
        state,
        reason,
        clients: None,
        status: None,
        peers: None,
        speed_down: None,
        speed_up: None,
        downloaded: None,
        uploaded: None,
    }
}

fn unavailable(why: &str) -> AcestreamLive {
    empty(LiveState::Unavailable, Some(why.into()))
}

/// Ask acexy about `ace` (per-stream status); while it is serving the stream, read the stats of
/// acexy's engine session.
pub async fn acestream_live(acexy: &str, engine: &str, ace: &str) -> AcestreamLive {
    let url = format!(
        "{}/ace/status?id={}",
        acexy.trim_end_matches('/'),
        url::form_urlencoded::byte_serialize(ace.as_bytes()).collect::<String>()
    );
    let (status, body) = match connection::fetch_with(&CLIENT, &url, true, TIMEOUT).await {
        Ok(r) => r,
        Err(e) => return unavailable(&format!("acexy did not answer: {e}")),
    };
    if status == 404 {
        return empty(LiveState::Inactive, None);
    }
    if (400..500).contains(&status) {
        return unavailable(&format!("this acexy version has no per-stream status (HTTP {status})"));
    }
    if !(200..300).contains(&status) {
        return unavailable(&format!("acexy status: HTTP {status}"));
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return unavailable("acexy status is not JSON");
    };
    let clients = v.get("clients").and_then(Value::as_u64);
    let stat_url = v.get("stat_url").and_then(Value::as_str);
    if clients.is_none() && stat_url.is_none() {
        // An acexy without per-stream status answers with its global status; a global stream
        // count does not tell whether this channel is the one being watched.
        return unavailable("this acexy version has no per-stream status");
    }
    if clients == Some(0) {
        return empty(LiveState::Inactive, None);
    }
    let mut live = empty(LiveState::Active, None);
    live.clients = clients;
    let Some(stat_url) = stat_url else {
        live.reason = Some("acexy reports no session statistics for this stream".into());
        return live;
    };
    match read_stat(stat_url, engine).await {
        Ok(Some(s)) => {
            let n = |k: &str| s.get(k).and_then(Value::as_u64);
            live.status = s.get("status").and_then(Value::as_str).map(str::to_string);
            live.peers = n("peers");
            live.speed_down = n("speed_down");
            live.speed_up = n("speed_up");
            live.downloaded = n("downloaded");
            live.uploaded = n("uploaded");
        }
        // The session ended between the two requests.
        Ok(None) => return empty(LiveState::Inactive, None),
        Err(e) => live.reason = Some(format!("session statistics unavailable: {e}")),
    }
    live
}

/// The `response` of a stat URL, moved onto the configured engine. Ok(None): the session is gone.
async fn read_stat(stat_url: &str, engine: &str) -> std::result::Result<Option<Value>, String> {
    let url = crate::probe::rebase(stat_url, engine).ok_or("acexy reported an unusable statistics URL")?;
    // Only the statistics endpoint, never a playback or command URL.
    let path = url::Url::parse(&url).map(|u| u.path().to_string()).unwrap_or_default();
    if !path.contains("/ace/stat/") {
        return Err("acexy reported an unexpected statistics URL".into());
    }
    let (status, body) = connection::fetch_with(&CLIENT, &url, true, TIMEOUT).await?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("engine: HTTP {status}"));
    }
    let v: Value = serde_json::from_slice(&body).map_err(|_| "the engine's answer is not JSON".to_string())?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let text = err.as_str().map_or_else(|| err.to_string(), str::to_string);
        // The engine reports an ended session as an error ("unknown playback session id").
        if text.to_ascii_lowercase().contains("unknown") {
            return Ok(None);
        }
        return Err(format!("engine error: {text}"));
    }
    Ok(Some(v.get("response").cloned().unwrap_or(Value::Null)))
}

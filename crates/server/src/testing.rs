//! A stub gateway for tests (feature `test-support`): speaks contract v1 with two rules.
//!
//! - `acestream`: 40-hex ids, `acestream://` links and `/ace/getstream?id=` URLs. Identity
//!   `ace:<id>`, ref `acestream://<id>`, play `<acexy>/ace/getstream?id=<id>` (absolute). Titles come
//!   and other hints from [`StubGateway::set_hints`]. Capabilities: observe, check.
//! - `vk`: `vk.com/video<o>_<v>[?list=]` (also vkvideo.ru). Identity `vk:<o>_<v>`, ref the canonical
//!   vk.com URL with `list`, play `play/vk.m3u8?ref=<encoded ref>` (relative). Capability: observe.
//!
//! Tests can make the stub fail, change rule revisions and the acexy address, set check outcomes,
//! and count calls.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gateway_contract::{
    CONTRACT, Capability, CheckOutcome, CheckRequest, CheckResponse, ContentState, ErrorBody, ErrorDetail, Fact, Hints,
    Info, MatchRequest, MatchResponse, MatchResult, Matched, Observation, ObserveRequest, ObserveResponse, RuleInfo,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    None,
    /// Every call answers HTTP 500.
    ServerError,
    /// `/v1/info` reports another contract version.
    WrongContract,
    /// `/v1/match` answers its results in reverse order.
    ReorderMatch,
    /// `/v1/match` answers an item error for every input.
    ItemErrors,
    /// `/v1/check` answers for another ref.
    WrongCheckRef,
}

struct Inner {
    acexy: String,
    match_revision: String,
    play_revision: String,
    failure: Failure,
    hints: HashMap<String, Hints>,
    outcome: CheckOutcome,
    content_state: Option<ContentState>,
    calls: HashMap<&'static str, usize>,
}

#[derive(Clone)]
pub struct StubGateway {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
}

impl StubGateway {
    /// Start the stub on a random local port.
    pub async fn start() -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            acexy: "http://acexy:8080".into(),
            match_revision: "1".into(),
            play_revision: "1".into(),
            failure: Failure::None,
            hints: HashMap::new(),
            outcome: CheckOutcome::Ok,
            content_state: None,
            calls: HashMap::new(),
        }));
        let app = Router::new()
            .route("/v1/info", get(info))
            .route("/v1/match", post(matching))
            .route("/v1/observe", post(observe))
            .route("/v1/check", post(check))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        StubGateway {
            url: format!("http://{addr}"),
            inner,
        }
    }

    pub fn fail(&self, f: Failure) {
        self.inner.lock().unwrap().failure = f;
    }

    /// Where acestream play URLs point (changes the play revision too).
    pub fn set_acexy(&self, base: &str, play_revision: &str) {
        let mut i = self.inner.lock().unwrap();
        i.acexy = base.into();
        i.play_revision = play_revision.into();
    }

    pub fn set_match_revision(&self, r: &str) {
        self.inner.lock().unwrap().match_revision = r.into();
    }

    pub fn set_title(&self, ace_id: &str, title: &str) {
        self.set_hints(
            ace_id,
            Hints {
                title: Some(title.into()),
                ..Default::default()
            },
        );
    }

    /// Hints the acestream rule returns for an id.
    pub fn set_hints(&self, ace_id: &str, hints: Hints) {
        self.inner.lock().unwrap().hints.insert(ace_id.into(), hints);
    }

    pub fn set_check(&self, outcome: CheckOutcome, content_state: Option<ContentState>) {
        let mut i = self.inner.lock().unwrap();
        i.outcome = outcome;
        i.content_state = content_state;
    }

    pub fn calls(&self, op: &str) -> usize {
        self.inner.lock().unwrap().calls.get(op).copied().unwrap_or(0)
    }
}

type St = State<Arc<Mutex<Inner>>>;

fn failing(i: &mut Inner, op: &'static str) -> Option<Response> {
    *i.calls.entry(op).or_default() += 1;
    (i.failure == Failure::ServerError).then(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: "boom".into(),
                    message: "the stub was told to fail".into(),
                },
            }),
        )
            .into_response()
    })
}

async fn info(State(s): St) -> Response {
    let mut i = s.lock().unwrap();
    if let Some(r) = failing(&mut i, "info") {
        return r;
    }
    Json(Info {
        name: "stub".into(),
        version: "0.0.0".into(),
        contract: if i.failure == Failure::WrongContract {
            CONTRACT + 1
        } else {
            CONTRACT
        },
        rules: vec![
            RuleInfo {
                name: "acestream".into(),
                title: "Acestream".into(),
                input_hint: Some("40-hex id or acestream:// link".into()),
                capabilities: vec![Capability::Observe, Capability::Check],
                match_revision: i.match_revision.clone(),
                play_revision: i.play_revision.clone(),
            },
            RuleInfo {
                name: "vk".into(),
                title: "VK video".into(),
                input_hint: Some("vk.com video link".into()),
                capabilities: vec![Capability::Observe],
                match_revision: i.match_revision.clone(),
                play_revision: i.play_revision.clone(),
            },
        ],
    })
    .into_response()
}

fn ace_id(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s
        .strip_prefix("acestream://")
        .or_else(|| s.split("/ace/getstream?id=").nth(1))
        .unwrap_or(s);
    let id: String = s.chars().take(40).collect();
    (id.len() == 40 && id.chars().all(|c| c.is_ascii_hexdigit()) && s.len() == 40).then(|| id.to_ascii_lowercase())
}

fn vk(input: &str) -> Option<(String, String)> {
    let u = url::Url::parse(input.trim()).ok()?;
    let host = u.host_str()?;
    if !(host.ends_with("vk.com") || host.ends_with("vkvideo.ru")) {
        return None;
    }
    let ov = u.path().strip_prefix("/video")?.to_string();
    let (o, v) = ov.split_once('_')?;
    o.parse::<i64>().ok()?;
    v.parse::<i64>().ok()?;
    let list = u.query_pairs().find(|(k, _)| k == "list").map(|(_, v)| v.into_owned());
    let reference = match list {
        Some(l) => format!("https://vk.com/video{ov}?list={l}"),
        None => format!("https://vk.com/video{ov}"),
    };
    Some((format!("vk:{ov}"), reference))
}

async fn matching(State(s): St, Json(req): Json<MatchRequest>) -> Response {
    let mut i = s.lock().unwrap();
    if let Some(r) = failing(&mut i, "match") {
        return r;
    }
    if i.failure == Failure::ItemErrors {
        let results = req
            .inputs
            .iter()
            .map(|input| MatchResult::Error {
                input: input.clone(),
                error: ErrorDetail {
                    code: "engine_error".into(),
                    message: "the engine failed".into(),
                },
            })
            .collect();
        return Json(MatchResponse { results }).into_response();
    }
    let reorder = i.failure == Failure::ReorderMatch;
    let mut results: Vec<MatchResult> = req
        .inputs
        .iter()
        .map(|input| match req.rule.as_str() {
            "acestream" => match ace_id(input) {
                Some(id) => MatchResult::Matched(Matched {
                    input: input.clone(),
                    identity: format!("ace:{id}"),
                    reference: format!("acestream://{id}"),
                    play: format!("{}/ace/getstream?id={id}", i.acexy),
                    hints: i.hints.get(&id).cloned().unwrap_or_default(),
                    warnings: vec![],
                }),
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some("not an Acestream id or link".into()),
                },
            },
            "vk" => match vk(input) {
                Some((identity, reference)) => MatchResult::Matched(Matched {
                    input: input.clone(),
                    identity,
                    play: format!(
                        "play/vk.m3u8?ref={}",
                        url::form_urlencoded::byte_serialize(reference.as_bytes()).collect::<String>()
                    ),
                    reference,
                    hints: Hints {
                        title: None,
                        region: None,
                        group: Some("VK".into()),
                    },
                    warnings: vec![],
                }),
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some("not a VK video link".into()),
                },
            },
            other => MatchResult::Error {
                input: input.clone(),
                error: ErrorDetail {
                    code: "unknown_rule".into(),
                    message: format!("no rule {other}"),
                },
            },
        })
        .collect();
    if reorder {
        results.reverse();
    }
    Json(MatchResponse { results }).into_response()
}

async fn observe(State(s): St, Json(req): Json<ObserveRequest>) -> Response {
    let mut i = s.lock().unwrap();
    if let Some(r) = failing(&mut i, "observe") {
        return r;
    }
    let results = req
        .refs
        .iter()
        .map(|r| Observation {
            reference: r.clone(),
            available: true,
            reason: None,
            content_state: i.content_state,
            watching: Some(false),
            facts: vec![Fact {
                label: "Peers".into(),
                value: "12".into(),
            }],
            title: Some("Observed".into()),
            raw: None,
        })
        .collect();
    Json(ObserveResponse { results }).into_response()
}

async fn check(State(s): St, Json(req): Json<CheckRequest>) -> Response {
    let mut i = s.lock().unwrap();
    if let Some(r) = failing(&mut i, "check") {
        return r;
    }
    let skipped = i.outcome == CheckOutcome::Skipped && !req.force;
    let outcome = if req.force && i.outcome == CheckOutcome::Skipped {
        CheckOutcome::Ok
    } else {
        i.outcome
    };
    let wrong = i.failure == Failure::WrongCheckRef;
    Json(CheckResponse {
        reference: if wrong {
            "acestream://other".into()
        } else {
            req.reference
        },
        outcome,
        reason: skipped.then(|| "busy".into()),
        content_state: i.content_state,
        detail: Some(serde_json::json!({ "force": req.force })),
        checked_at: chrono::Utc::now().to_rfc3339(),
    })
    .into_response()
}

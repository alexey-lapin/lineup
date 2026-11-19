//! A stub gateway for tests (feature `test-support`): speaks contract v1 with two rules.
//!
//! - `ids`: 40-hex ids, `id://` links and `/stream?id=` URLs. Identity `id:<id>`, ref `id://<id>`,
//!   play `<upstream>/stream?id=<id>` (absolute, pointing past the gateway). Titles and other hints
//!   come from [`StubGateway::set_hints`]. Capabilities: observe, check.
//! - `pages`: `video.example/v/<n>[?key=]` links (also on subdomains). Identity `page:<n>`, ref the
//!   canonical `https://video.example/v/<n>` with `key`, play `play/pages.m3u8?ref=<encoded ref>`
//!   (relative, served by the gateway). Capability: observe.
//!
//! Tests can make the stub fail, change rule revisions and the upstream address, set check
//! outcomes, and count calls.

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
    upstream: String,
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
            upstream: "http://upstream:8080".into(),
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

    /// Where `ids` play URLs point (changes the play revision too).
    pub fn set_upstream(&self, base: &str, play_revision: &str) {
        let mut i = self.inner.lock().unwrap();
        i.upstream = base.into();
        i.play_revision = play_revision.into();
    }

    pub fn set_match_revision(&self, r: &str) {
        self.inner.lock().unwrap().match_revision = r.into();
    }

    pub fn set_title(&self, id: &str, title: &str) {
        self.set_hints(
            id,
            Hints {
                title: Some(title.into()),
                ..Default::default()
            },
        );
    }

    /// Hints the `ids` rule returns for an id.
    pub fn set_hints(&self, id: &str, hints: Hints) {
        self.inner.lock().unwrap().hints.insert(id.into(), hints);
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
                name: "ids".into(),
                title: "Ids".into(),
                input_hint: Some("40-hex id or id:// link".into()),
                capabilities: vec![Capability::Observe, Capability::Check],
                match_revision: i.match_revision.clone(),
                play_revision: i.play_revision.clone(),
            },
            RuleInfo {
                name: "pages".into(),
                title: "Video pages".into(),
                input_hint: Some("video.example link".into()),
                capabilities: vec![Capability::Observe],
                match_revision: i.match_revision.clone(),
                play_revision: i.play_revision.clone(),
            },
        ],
    })
    .into_response()
}

fn stream_id(input: &str) -> Option<String> {
    let s = input.trim();
    let s = s
        .strip_prefix("id://")
        .or_else(|| s.split("/stream?id=").nth(1))
        .unwrap_or(s);
    let id: String = s.chars().take(40).collect();
    (id.len() == 40 && id.chars().all(|c| c.is_ascii_hexdigit()) && s.len() == 40).then(|| id.to_ascii_lowercase())
}

/// Identity and canonical ref of a `video.example` page link.
fn page(input: &str) -> Option<(String, String)> {
    let u = url::Url::parse(input.trim()).ok()?;
    let host = u.host_str()?;
    if !(host == "video.example" || host.ends_with(".video.example")) {
        return None;
    }
    let n = u.path().strip_prefix("/v/")?.to_string();
    n.parse::<u64>().ok()?;
    let key = u.query_pairs().find(|(k, _)| k == "key").map(|(_, v)| v.into_owned());
    let reference = match key {
        Some(k) => format!("https://video.example/v/{n}?key={k}"),
        None => format!("https://video.example/v/{n}"),
    };
    Some((format!("page:{n}"), reference))
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
                    code: "upstream_error".into(),
                    message: "the upstream failed".into(),
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
            "ids" => match stream_id(input) {
                Some(id) => MatchResult::Matched(Matched {
                    input: input.clone(),
                    identity: format!("id:{id}"),
                    reference: format!("id://{id}"),
                    play: format!("{}/stream?id={id}", i.upstream),
                    hints: i.hints.get(&id).cloned().unwrap_or_default(),
                    warnings: vec![],
                }),
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some("not an id or id:// link".into()),
                },
            },
            "pages" => match page(input) {
                Some((identity, reference)) => MatchResult::Matched(Matched {
                    input: input.clone(),
                    identity,
                    play: format!(
                        "play/pages.m3u8?ref={}",
                        url::form_urlencoded::byte_serialize(reference.as_bytes()).collect::<String>()
                    ),
                    reference,
                    hints: Hints {
                        title: None,
                        region: None,
                        group: Some("Video".into()),
                    },
                    warnings: vec![],
                }),
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some("not a video.example link".into()),
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
        reference: if wrong { "id://other".into() } else { req.reference },
        outcome,
        reason: skipped.then(|| "busy".into()),
        content_state: i.content_state,
        detail: Some(serde_json::json!({ "force": req.force })),
        checked_at: chrono::Utc::now().to_rfc3339(),
    })
    .into_response()
}

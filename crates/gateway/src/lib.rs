//! lineup-gateway: serves the gateway contract (`/v1/*`) for its rules, and relays page streams
//! (`/play/*`).

pub mod config;
pub mod extract;
pub mod hls;
pub mod net;
pub mod relay;
pub mod rules;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gateway_contract::{
    CONTRACT, Capability, CheckOutcome, CheckRequest, CheckResponse, ContentState, ErrorBody, ErrorDetail, Fact, Info,
    MAX_MATCH_INPUTS, MAX_OBSERVE_REFS, MatchRequest, MatchResponse, Observation, ObserveRequest, ObserveResponse,
    RuleInfo,
};
use serde::Deserialize;

use crate::config::Config;
use crate::extract::{ExtractError, Extractor, Priority, Tools};
use crate::net::{Allow, EgressProxy};
use crate::relay::{Relay, text};
use crate::rules::acestream::{Acestream, ProbeTiming};
use crate::rules::vk;

pub struct Gateway {
    pub acestream: Option<Acestream>,
    pub vk: Option<Relay>,
    /// Kept alive while the gateway runs.
    _proxy: Option<EgressProxy>,
}

pub type AppState = Arc<Gateway>;

impl Gateway {
    /// Build the gateway from its config: start the egress proxy, check the tools.
    pub async fn new(cfg: &Config, timing: ProbeTiming) -> anyhow::Result<Arc<Self>> {
        let allow = Allow::new(&cfg.egress.allow);
        let acestream = cfg.acestream.clone().map(|a| Acestream::new(a, timing));
        let (vk, proxy) = match &cfg.vk {
            None => (None, None),
            Some(_) => {
                let proxy = EgressProxy::start(allow.clone()).await?;
                tracing::info!("egress proxy for the extractors on {}", proxy.url());
                for (name, bin) in [("yt_dlp", &cfg.tools.yt_dlp), ("streamlink", &cfg.tools.streamlink)] {
                    if let Some(b) = bin
                        && !b.exists()
                    {
                        anyhow::bail!("tools.{name}: {} does not exist", b.display());
                    }
                }
                let extractor = Arc::new(Extractor::new(
                    Tools {
                        yt_dlp: cfg.tools.yt_dlp.clone(),
                        streamlink: cfg.tools.streamlink.clone(),
                        proxy: proxy.url(),
                        timeout: cfg.limits.extraction_timeout(),
                    },
                    cfg.limits.extractions,
                ));
                let client = net::public_client(&allow, cfg.limits.upstream_timeout());
                (
                    Some(Relay::new(client, allow.clone(), extractor, &cfg.limits)),
                    Some(proxy),
                )
            }
        };
        Ok(Arc::new(Gateway {
            acestream,
            vk,
            _proxy: proxy,
        }))
    }

    pub fn info(&self) -> Info {
        let mut rules = Vec::new();
        if let Some(a) = &self.acestream {
            rules.push(RuleInfo {
                name: "acestream".into(),
                title: "Acestream".into(),
                input_hint: Some("40-hex content id, acestream:// link or acexy/engine URL".into()),
                capabilities: if a.can_check() {
                    vec![Capability::Observe, Capability::Check]
                } else {
                    vec![Capability::Observe]
                },
                match_revision: rules::acestream::MATCH_REVISION.into(),
                play_revision: a.play_revision(),
            });
        }
        if self.vk.is_some() {
            rules.push(RuleInfo {
                name: "vk".into(),
                title: "VK video".into(),
                input_hint: Some("vk.com, vkvideo.ru or vksport link to a video or broadcast".into()),
                capabilities: vec![Capability::Observe, Capability::Check],
                match_revision: vk::MATCH_REVISION.into(),
                play_revision: vk::PLAY_REVISION.into(),
            });
        }
        Info {
            name: "lineup-gateway".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            contract: CONTRACT,
            rules,
        }
    }
}

pub fn router(g: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/info", get(info))
        .route("/v1/match", post(matching))
        .route("/v1/observe", post(observe))
        .route("/v1/check", post(check))
        .route("/play/{file}", get(play).head(play))
        .route("/play/s/{sid}/{file}", get(resource).head(resource))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn(log_request))
        .with_state(g)
}

async fn log_request(req: Request, next: axum::middleware::Next) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_owned());
    let started = std::time::Instant::now();
    let res = next.run(req).await;
    let status = res.status().as_u16();
    let ms = started.elapsed().as_millis();
    if path.starts_with("/play/s/") && status < 400 {
        tracing::debug!("{method} {path} {status} {ms} ms");
    } else {
        tracing::info!("{method} {path} {status} {ms} ms");
    }
    res
}

fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
            },
        }),
    )
        .into_response()
}

fn unknown_rule(rule: &str) -> Response {
    error(StatusCode::NOT_FOUND, "unknown_rule", format!("no rule named {rule:?}"))
}

async fn info(State(g): State<AppState>) -> Json<Info> {
    Json(g.info())
}

async fn matching(State(g): State<AppState>, Json(req): Json<MatchRequest>) -> Response {
    if req.inputs.len() > MAX_MATCH_INPUTS {
        return error(
            StatusCode::BAD_REQUEST,
            "too_many_inputs",
            format!("at most {MAX_MATCH_INPUTS} inputs per request"),
        );
    }
    let results = match req.rule.as_str() {
        "acestream" => match &g.acestream {
            Some(a) => a.match_inputs(&req.inputs).await,
            None => return unknown_rule(&req.rule),
        },
        "vk" if g.vk.is_some() => vk::match_inputs(&req.inputs),
        other => return unknown_rule(other),
    };
    Json(MatchResponse { results }).into_response()
}

async fn observe(State(g): State<AppState>, Json(req): Json<ObserveRequest>) -> Response {
    if req.refs.len() > MAX_OBSERVE_REFS {
        return error(
            StatusCode::BAD_REQUEST,
            "too_many_refs",
            format!("at most {MAX_OBSERVE_REFS} refs per request"),
        );
    }
    let mut results = Vec::with_capacity(req.refs.len());
    match req.rule.as_str() {
        "acestream" => {
            let Some(a) = &g.acestream else {
                return unknown_rule(&req.rule);
            };
            for r in &req.refs {
                results.push(a.observe(r).await);
            }
        }
        "vk" => {
            let Some(relay) = &g.vk else {
                return unknown_rule(&req.rule);
            };
            for r in &req.refs {
                results.push(observe_page(relay, r).await);
            }
        }
        other => return unknown_rule(other),
    }
    Json(ObserveResponse { results }).into_response()
}

async fn observe_page(relay: &Relay, reference: &str) -> Observation {
    let mut o = Observation {
        reference: reference.to_string(),
        available: false,
        reason: None,
        content_state: None,
        watching: None,
        facts: vec![],
        title: None,
        raw: None,
    };
    let Some(r) = vk::parse(reference) else {
        o.reason = Some("not a VK reference".into());
        return o;
    };
    match relay.probe(&r.canonical(), Priority::Background).await {
        Ok((d, summary)) => {
            o.available = true;
            o.content_state = Some(if summary.ended {
                ContentState::Recording
            } else {
                ContentState::Live
            });
            o.title = d.title.clone();
            o.facts.push(Fact {
                label: "Found by".into(),
                value: d.tool.into(),
            });
            if let Some(secs) = d.duration.filter(|_| summary.ended) {
                o.facts.push(Fact {
                    label: "Duration".into(),
                    value: hms(secs),
                });
            }
        }
        Err(ExtractError::NoStream(m)) => {
            o.available = true;
            o.content_state = Some(ContentState::EndedProcessing);
            o.reason = Some(m);
        }
        Err(e) => o.reason = Some(e.to_string()),
    }
    o
}

fn hms(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

async fn check(State(g): State<AppState>, Json(req): Json<CheckRequest>) -> Response {
    let resp = match req.rule.as_str() {
        "acestream" => {
            let Some(a) = &g.acestream else {
                return unknown_rule(&req.rule);
            };
            a.check(&req.reference, req.force).await
        }
        "vk" => {
            let Some(relay) = &g.vk else {
                return unknown_rule(&req.rule);
            };
            check_page(relay, &req.reference).await
        }
        other => return unknown_rule(other),
    };
    Json(resp).into_response()
}

async fn check_page(relay: &Relay, reference: &str) -> CheckResponse {
    let mut resp = CheckResponse {
        reference: reference.to_string(),
        outcome: CheckOutcome::Unknown,
        reason: None,
        content_state: None,
        detail: None,
        checked_at: chrono::Utc::now().to_rfc3339(),
    };
    let Some(r) = vk::parse(reference) else {
        resp.reason = Some("bad_ref".into());
        return resp;
    };
    match relay.probe(&r.canonical(), Priority::Background).await {
        Ok((d, summary)) => {
            resp.outcome = CheckOutcome::Ok;
            resp.content_state = Some(if summary.ended {
                ContentState::Recording
            } else {
                ContentState::Live
            });
            resp.detail = Some(serde_json::json!({ "tool": d.tool, "segments": summary.segments }));
        }
        Err(ExtractError::NotFound(m)) => {
            resp.outcome = CheckOutcome::Dead;
            resp.reason = Some("not_found".into());
            resp.detail = Some(serde_json::json!({ "error": m }));
        }
        Err(ExtractError::NoStream(m)) => {
            resp.content_state = Some(ContentState::EndedProcessing);
            resp.reason = Some("no_stream_yet".into());
            resp.detail = Some(serde_json::json!({ "error": m }));
        }
        Err(ExtractError::Failed(m)) => {
            resp.reason = Some("lookup_failed".into());
            resp.detail = Some(serde_json::json!({ "error": m }));
        }
    }
    resp
}

#[derive(Deserialize)]
struct PlayQuery {
    #[serde(rename = "ref")]
    reference: Option<String>,
}

async fn play(
    State(g): State<AppState>,
    method: Method,
    Path(file): Path<String>,
    Query(q): Query<PlayQuery>,
) -> Response {
    let head = method == Method::HEAD;
    match file.as_str() {
        "vk.m3u8" => {
            let Some(relay) = &g.vk else {
                return text(StatusCode::NOT_FOUND, "the vk rule is off");
            };
            let Some(r) = q.reference.as_deref().and_then(vk::parse) else {
                return text(StatusCode::BAD_REQUEST, "ref must be a VK video link");
            };
            relay.play(&r.canonical(), head).await
        }
        _ => text(StatusCode::NOT_FOUND, "no such play route"),
    }
}

async fn resource(
    State(g): State<AppState>,
    method: Method,
    Path((sid, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(relay) = &g.vk else {
        return text(StatusCode::NOT_FOUND, "no such session");
    };
    relay.resource(&sid, &file, &headers, method == Method::HEAD).await
}

/// Run the gateway until SIGINT/SIGTERM.
pub async fn serve(cfg: Config) -> anyhow::Result<()> {
    let g = Gateway::new(&cfg, ProbeTiming::default()).await?;
    let names: Vec<String> = g.info().rules.into_iter().map(|r| r.name).collect();
    let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
    tracing::info!(
        "lineup-gateway listening on http://{} (rules: {})",
        listener.local_addr()?,
        names.join(", ")
    );
    axum::serve(listener, router(g))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

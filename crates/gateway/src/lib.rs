//! lineup-gateway: serves the gateway contract (`/v1/*`) for its configured rules, and relays the
//! streams of page rules (`/play/*`).

pub mod config;
pub mod extract;
pub mod hls;
pub mod net;
pub mod relay;
pub mod rules;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gateway_contract::{
    CONTRACT, CheckRequest, ErrorBody, ErrorDetail, Info, MAX_MATCH_INPUTS, MAX_OBSERVE_REFS, MatchRequest,
    MatchResponse, ObserveRequest, ObserveResponse,
};
use serde::Deserialize;

use crate::config::{Config, RuleConfig};
use crate::extract::{Extractor, RelayPolicy, Tool};
use crate::net::{Allow, EgressProxy};
use crate::relay::{Relay, text};
use crate::rules::Rule;
use crate::rules::acestream::{Acestream, EngineLock, ProbeTiming};
use crate::rules::page::PageRule;

pub struct Gateway {
    /// Rules by name, in name order.
    rules: Vec<(String, Rule)>,
    /// The relay every page rule plays through; present when there is one.
    relay: Option<Relay>,
    /// Kept alive while the gateway runs.
    _proxy: Option<EgressProxy>,
}

pub type AppState = Arc<Gateway>;

impl Gateway {
    /// Build the gateway from its config: start the egress proxy, check the extractors.
    pub async fn new(cfg: &Config, timing: ProbeTiming) -> anyhow::Result<Arc<Self>> {
        let allow = Allow::new(&cfg.egress.allow);
        let pages = cfg.rules.values().any(|r| matches!(r, RuleConfig::Vk(_)));
        let (relay, proxy) = if pages {
            let proxy = EgressProxy::start(allow.clone()).await?;
            tracing::info!("egress proxy for the extractors on {}", proxy.url());
            let extractor = Extractor::new(proxy.url(), cfg.limits.extractions);
            let client = net::public_client(&allow, cfg.limits.upstream_timeout());
            (
                Some(Relay::new(client, allow.clone(), extractor, &cfg.limits)),
                Some(proxy),
            )
        } else {
            (None, None)
        };
        // Extractors that some rule uses: checked once, shared by every rule naming them.
        let mut tools: HashMap<&str, Arc<Tool>> = HashMap::new();
        for r in cfg.rules.values() {
            let RuleConfig::Vk(p) = r else { continue };
            for name in &p.extractors {
                if tools.contains_key(name.as_str()) {
                    continue;
                }
                let x = &cfg.extractors[name];
                let version = tool_version(&x.command, &x.version_args)
                    .await
                    .map_err(|e| anyhow::anyhow!("extractors.{name}: {e}"))?;
                tracing::info!("extractors.{name}: {} {version}", x.command.display());
                tools.insert(
                    name.as_str(),
                    Arc::new(Tool::from_config(name, x, cfg.limits.extraction_timeout())),
                );
            }
        }
        let mut engines: HashMap<String, EngineLock> = HashMap::new();
        let mut rules = Vec::new();
        for (name, r) in &cfg.rules {
            let rule = match r {
                RuleConfig::Acestream(a) => {
                    let lock = engines.entry(Acestream::engine_key(a)).or_default().clone();
                    Rule::Acestream(Acestream::new(name, a.clone(), timing, lock))
                }
                RuleConfig::Vk(p) => {
                    let chain = p.extractors.iter().map(|x| tools[x.as_str()].clone()).collect();
                    let policy = RelayPolicy::new(chain, p.expired_status.clone(), p.expires_param.clone());
                    Rule::Page(PageRule::new(name, p, Arc::new(policy)))
                }
            };
            rules.push((name.clone(), rule));
        }
        Ok(Arc::new(Gateway {
            rules,
            relay,
            _proxy: proxy,
        }))
    }

    pub fn info(&self) -> Info {
        Info {
            name: "lineup-gateway".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            contract: CONTRACT,
            rules: self.rules.iter().map(|(_, r)| r.info()).collect(),
        }
    }

    fn rule(&self, name: &str) -> Option<&Rule> {
        self.rules.iter().find(|(n, _)| n == name).map(|(_, r)| r)
    }

    fn page_rule(&self, name: &str) -> Option<(&PageRule, &Relay)> {
        match (self.rule(name)?, &self.relay) {
            (Rule::Page(p), Some(relay)) => Some((p, relay)),
            _ => None,
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
    let results = match g.rule(&req.rule) {
        Some(Rule::Acestream(a)) => a.match_inputs(&req.inputs).await,
        Some(Rule::Page(p)) => p.match_inputs(&req.inputs),
        None => return unknown_rule(&req.rule),
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
    match g.rule(&req.rule) {
        Some(Rule::Acestream(a)) => {
            for r in &req.refs {
                results.push(a.observe(r).await);
            }
        }
        Some(Rule::Page(_)) => {
            let Some((p, relay)) = g.page_rule(&req.rule) else {
                return unknown_rule(&req.rule);
            };
            for r in &req.refs {
                results.push(p.observe(relay, r).await);
            }
        }
        None => return unknown_rule(&req.rule),
    }
    Json(ObserveResponse { results }).into_response()
}

/// `tool --version` (or the configured version arguments), which also proves the tool runs.
async fn tool_version(bin: &std::path::Path, args: &[String]) -> anyhow::Result<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{} {} did not finish", bin.display(), args.join(" ")))?
    .map_err(|e| anyhow::anyhow!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        anyhow::bail!("{} {} failed", bin.display(), args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn check(State(g): State<AppState>, Json(req): Json<CheckRequest>) -> Response {
    let resp = match g.rule(&req.rule) {
        Some(Rule::Acestream(a)) => a.check(&req.reference, req.force).await,
        Some(Rule::Page(_)) => {
            let Some((p, relay)) = g.page_rule(&req.rule) else {
                return unknown_rule(&req.rule);
            };
            p.check(relay, &req.reference).await
        }
        None => return unknown_rule(&req.rule),
    };
    Json(resp).into_response()
}

#[derive(Deserialize)]
struct PlayQuery {
    #[serde(rename = "ref")]
    reference: Option<String>,
}

/// `/play/<rule>.m3u8?ref=<ref>` for a page rule.
async fn play(
    State(g): State<AppState>,
    method: Method,
    Path(file): Path<String>,
    Query(q): Query<PlayQuery>,
) -> Response {
    let head = method == Method::HEAD;
    let Some((p, relay)) = file.strip_suffix(".m3u8").and_then(|name| g.page_rule(name)) else {
        return text(StatusCode::NOT_FOUND, "no such play route");
    };
    let Some(r) = q.reference.as_deref().and_then(|x| p.parse(x)) else {
        return text(StatusCode::BAD_REQUEST, "ref is not a reference this rule accepts");
    };
    relay.play(&p.lookup(&r), head).await
}

async fn resource(
    State(g): State<AppState>,
    method: Method,
    Path((sid, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(relay) = &g.relay else {
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
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// SIGINT or SIGTERM (what `docker stop` sends).
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}

/// For a container healthcheck: whether `/healthz` answers on the configured port.
pub async fn healthcheck(cfg: &Config) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{}/healthz", cfg.listen.port()))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

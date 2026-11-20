//! The `relay` kind: references are pages that extractors look up, played through the shared HLS
//! relay at `play/<rule>.m3u8?ref=<ref>`. Which pages, and their identities and refs, come from
//! the rule's patterns and templates.
//!
//! - observe: a lookup and the first media playlist (background priority).
//! - check: the same, as an outcome: live or a recording is ok, a page without a video is dead,
//!   an ended broadcast whose recording is not ready yet is inconclusive.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use gateway_contract::{
    Capability, CheckOutcome, CheckResponse, ContentState, Fact, Hints, MatchResult, Matched, Observation, RuleInfo,
};

use crate::config::{RelayConfig, RuleConfig};
use crate::extract::{Descriptor, ExtractError, Lookup, Priority, RelayPolicy};
use crate::hls::Summary;
use crate::matcher::{Match, Matcher};
use crate::relay::{Relay, text};
use crate::rules::Rule;

pub struct PageRule {
    name: String,
    config: RuleConfig,
    title: String,
    input_hint: Option<String>,
    group: Option<String>,
    matcher: Matcher,
    revisions: (String, String),
    policy: Arc<RelayPolicy>,
    relay: Arc<Relay>,
}

impl PageRule {
    pub fn new(name: &str, cfg: &RelayConfig, policy: Arc<RelayPolicy>, relay: Arc<Relay>) -> anyhow::Result<Self> {
        let config = RuleConfig::Relay(cfg.clone());
        Ok(PageRule {
            name: name.to_string(),
            title: cfg.title.clone().unwrap_or_else(|| name.to_string()),
            input_hint: cfg.input_hint.clone(),
            group: cfg.group.clone(),
            matcher: config.matcher(name)?,
            revisions: config.revisions(name),
            config,
            policy,
            relay,
        })
    }

    fn parse(&self, input: &str) -> Option<Match> {
        self.matcher.matches(input)
    }

    /// The lookup of a matched reference under this rule's policy: its canonical ref.
    fn lookup(&self, m: &Match) -> Lookup {
        Lookup::new(m.reference.clone(), self.policy.clone())
    }

    async fn observe_ref(&self, reference: &str) -> Observation {
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
        let Some(m) = self.parse(reference) else {
            o.reason = Some(format!("not a {} reference", self.title));
            return o;
        };
        match self.relay.probe(&self.lookup(&m), Priority::Background).await {
            Ok((d, summary)) => {
                o.available = true;
                o.content_state = Some(page_state(&d, &summary));
                o.title = d.title.clone();
                o.facts.push(Fact {
                    label: "Found by".into(),
                    value: d.tool.clone(),
                });
                if let Some(secs) = d.duration.filter(|_| o.content_state == Some(ContentState::Recording)) {
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

    async fn check_ref(&self, reference: &str) -> CheckResponse {
        let mut resp = CheckResponse {
            reference: reference.to_string(),
            outcome: CheckOutcome::Unknown,
            reason: None,
            content_state: None,
            detail: None,
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        let Some(m) = self.parse(reference) else {
            resp.reason = Some("bad_ref".into());
            return resp;
        };
        match self.relay.probe(&self.lookup(&m), Priority::Background).await {
            Ok((d, summary)) => {
                let state = page_state(&d, &summary);
                resp.content_state = Some(state);
                if state == ContentState::EndedProcessing {
                    // Ended, recording not ready: inconclusive, never a failure.
                    resp.reason = Some("recording_not_ready".into());
                } else {
                    resp.outcome = CheckOutcome::Ok;
                }
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
}

impl Rule for PageRule {
    fn info(&self) -> RuleInfo {
        RuleInfo {
            name: self.name.clone(),
            title: self.title.clone(),
            input_hint: self.input_hint.clone(),
            capabilities: vec![Capability::Observe, Capability::Check],
            match_revision: self.revisions.0.clone(),
            play_revision: self.revisions.1.clone(),
        }
    }

    fn match_inputs<'a>(&'a self, inputs: &'a [String]) -> BoxFuture<'a, Vec<MatchResult>> {
        let results = inputs
            .iter()
            .map(|input| match self.parse(input) {
                Some(m) => match self.config.play(&self.name, &m) {
                    Ok(play) => MatchResult::Matched(Matched {
                        input: input.clone(),
                        identity: m.identity,
                        reference: m.reference,
                        play,
                        hints: Hints {
                            title: None,
                            region: None,
                            group: self.group.clone(),
                        },
                        warnings: vec![],
                    }),
                    Err(e) => MatchResult::NoMatch {
                        input: input.clone(),
                        reason: Some(e.to_string()),
                    },
                },
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some(format!("not a {} link", self.title)),
                },
            })
            .collect();
        std::future::ready(results).boxed()
    }

    fn observe<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Observation> {
        self.observe_ref(reference).boxed()
    }

    fn check<'a>(&'a self, reference: &'a str, _force: bool) -> BoxFuture<'a, CheckResponse> {
        self.check_ref(reference).boxed()
    }

    fn play<'a>(&'a self, reference: &'a str, head: bool) -> Option<BoxFuture<'a, Response>> {
        Some(
            async move {
                match self.parse(reference) {
                    Some(m) => self.relay.play(&self.lookup(&m), head).await,
                    None => text(StatusCode::BAD_REQUEST, "ref is not a reference this rule accepts"),
                }
            }
            .boxed(),
        )
    }
}

/// What a page is now, from its lookup and its first media playlist. ENDLIST alone is not a
/// recording: a lookup that still says live, or a playlist without segments, means the broadcast
/// ended and its recording is not playable yet.
fn page_state(d: &Descriptor, s: &Summary) -> ContentState {
    if !s.ended {
        ContentState::Live
    } else if d.is_live == Some(true) || s.segments == 0 {
        ContentState::EndedProcessing
    } else {
        ContentState::Recording
    }
}

fn hms(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

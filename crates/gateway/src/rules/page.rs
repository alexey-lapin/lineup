//! Page rules: references are pages that extractors look up, played through the shared HLS relay
//! at `play/<rule>.m3u8?ref=<ref>`.
//!
//! - observe: a lookup and the first media playlist (background priority).
//! - check: the same, as an outcome: live or a recording is ok, a page without a video is dead,
//!   an ended broadcast whose recording is not ready yet is inconclusive.

use std::sync::Arc;

use gateway_contract::{
    Capability, CheckOutcome, CheckResponse, ContentState, Fact, Hints, MatchResult, Matched, Observation, RuleInfo,
};

use crate::config::PageConfig;
use crate::extract::{Descriptor, ExtractError, Lookup, Priority, RelayPolicy};
use crate::hls::Summary;
use crate::relay::Relay;
use crate::rules::vk::{self, VkRef};

pub struct PageRule {
    pub name: String,
    title: String,
    input_hint: Option<String>,
    group: Option<String>,
    pub policy: Arc<RelayPolicy>,
}

impl PageRule {
    pub fn new(name: &str, cfg: &PageConfig, policy: Arc<RelayPolicy>) -> Self {
        PageRule {
            name: name.to_string(),
            title: cfg.title.clone().unwrap_or_else(|| name.to_string()),
            input_hint: cfg.input_hint.clone(),
            group: cfg.group.clone(),
            policy,
        }
    }

    pub fn info(&self) -> RuleInfo {
        RuleInfo {
            name: self.name.clone(),
            title: self.title.clone(),
            input_hint: self.input_hint.clone(),
            capabilities: vec![Capability::Observe, Capability::Check],
            match_revision: vk::MATCH_REVISION.into(),
            play_revision: vk::PLAY_REVISION.into(),
        }
    }

    pub fn parse(&self, input: &str) -> Option<VkRef> {
        vk::parse(&self.name, input)
    }

    /// The lookup of a parsed reference under this rule's policy.
    pub fn lookup(&self, r: &VkRef) -> Lookup {
        Lookup::new(r.canonical(), self.policy.clone())
    }

    pub fn match_inputs(&self, inputs: &[String]) -> Vec<MatchResult> {
        inputs
            .iter()
            .map(|input| match self.parse(input) {
                Some(r) => MatchResult::Matched(Matched {
                    input: input.clone(),
                    identity: r.identity(),
                    reference: r.canonical(),
                    play: r.play(&self.name),
                    hints: Hints {
                        title: None,
                        region: None,
                        group: self.group.clone(),
                    },
                    warnings: vec![],
                }),
                None => MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some(format!("not a {} link", self.title)),
                },
            })
            .collect()
    }

    pub async fn observe(&self, relay: &Relay, reference: &str) -> Observation {
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
        let Some(r) = self.parse(reference) else {
            o.reason = Some(format!("not a {} reference", self.title));
            return o;
        };
        match relay.probe(&self.lookup(&r), Priority::Background).await {
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

    pub async fn check(&self, relay: &Relay, reference: &str) -> CheckResponse {
        let mut resp = CheckResponse {
            reference: reference.to_string(),
            outcome: CheckOutcome::Unknown,
            reason: None,
            content_state: None,
            detail: None,
            checked_at: chrono::Utc::now().to_rfc3339(),
        };
        let Some(r) = self.parse(reference) else {
            resp.reason = Some("bad_ref".into());
            return resp;
        };
        match relay.probe(&self.lookup(&r), Priority::Background).await {
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

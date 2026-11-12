//! Wire types of the gateway contract, version 1.
//!
//! A gateway turns references that can't be played as-is (an Acestream id, a video page) into
//! playable URLs. lineup talks to it through four endpoints, all JSON:
//!
//! - `GET  /v1/info`: the gateway's rules and their revisions.
//! - `POST /v1/match`: validate and canonicalise inputs for one rule. Never starts a stream.
//! - `POST /v1/observe`: passive state of references (capability `observe`). Never starts a stream.
//! - `POST /v1/check`: active health check of one reference (capability `check`).
//!
//! Errors outside a per-item result use [`ErrorBody`] with a 4xx (bad request) or 5xx status.
//! The JSON files in `fixtures/` are the normative examples; tests keep these types in step with them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The contract version these types describe. A client refuses a gateway reporting another one.
pub const CONTRACT: u32 = 1;
/// Most inputs one `/v1/match` request may carry.
pub const MAX_MATCH_INPUTS: usize = 500;
/// Most references one `/v1/observe` request may carry.
pub const MAX_OBSERVE_REFS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub name: String,
    pub version: String,
    pub contract: u32,
    pub rules: Vec<RuleInfo>,
}

impl Info {
    pub fn rule(&self, name: &str) -> Option<&RuleInfo> {
        self.rules.iter().find(|r| r.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleInfo {
    /// Stable machine name, e.g. `acestream`, `vk`.
    pub name: String,
    /// Display name.
    pub title: String,
    /// What input the rule accepts, for UIs.
    #[serde(default)]
    pub input_hint: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Changes when the rule would match an input differently (identity, ref or hints).
    pub match_revision: String,
    /// Changes when the rule would return a different play reference for the same ref.
    pub play_revision: String,
}

impl RuleInfo {
    pub fn can(&self, c: Capability) -> bool {
        self.capabilities.contains(&c)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Observe,
    Check,
    /// A capability this client doesn't know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRequest {
    pub rule: String,
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchResponse {
    /// One result per input, in the same order (repeated inputs get repeated results).
    pub results: Vec<MatchResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MatchResult {
    Matched(Matched),
    /// The input is not something this rule accepts.
    NoMatch {
        input: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// The rule accepts the input's form but could not process this item.
    Error {
        input: String,
        error: ErrorDetail,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Matched {
    pub input: String,
    /// De-duplication key within the rule. Opaque.
    pub identity: String,
    /// Canonical, self-contained reference: everything the gateway needs to play, observe or
    /// check this item later, even after a restart.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Play reference: a path relative to the gateway's base URL (no leading slash), or an
    /// absolute http(s) URL. Durable: never a CDN or session URL.
    pub play: String,
    #[serde(default)]
    pub hints: Hints,
    /// Problems that did not stop the match, such as a failed title lookup.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Suggestions for a new channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hints {
    #[serde(default)]
    pub title: Option<String>,
    /// Two-letter lowercase region, e.g. `de`.
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// The item is usually a one-off event.
    #[serde(default)]
    pub event: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveRequest {
    pub rule: String,
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserveResponse {
    /// One result per ref, in the same order.
    pub results: Vec<Observation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    #[serde(rename = "ref")]
    pub reference: String,
    /// Whether anything could be observed.
    pub available: bool,
    /// Why not, or why some numbers are missing.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub content_state: Option<ContentState>,
    /// Whether someone is watching through the gateway's services right now, when known.
    #[serde(default)]
    pub watching: Option<bool>,
    /// Labelled values for display, in order.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// The item's current title, when the source reports one.
    #[serde(default)]
    pub title: Option<String>,
    /// Rule-specific raw data.
    #[serde(default)]
    pub raw: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentState {
    Live,
    /// The live broadcast ended and the recording is not playable yet.
    EndedProcessing,
    Recording,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRequest {
    pub rule: String,
    #[serde(rename = "ref")]
    pub reference: String,
    /// Check even if the item seems to be in use.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResponse {
    #[serde(rename = "ref")]
    pub reference: String,
    pub outcome: CheckOutcome,
    /// Machine-readable reason for `unknown`/`skipped` (e.g. `busy`, `guard_unavailable`).
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub content_state: Option<ContentState>,
    #[serde(default)]
    pub detail: Option<Value>,
    /// RFC 3339.
    pub checked_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// Playable.
    Ok,
    /// Not playable.
    Dead,
    /// Could not tell (the item, the gateway's services or the network failed).
    Unknown,
    /// Not checked, for example because someone is watching.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each fixture parses into its type and serialises back to the same JSON value.
    fn round_trip<T: serde::de::DeserializeOwned + Serialize>(json: &str) -> T {
        let value: Value = serde_json::from_str(json).unwrap();
        let typed: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), value);
        typed
    }

    #[test]
    fn fixtures() {
        let info: Info = round_trip(include_str!("../fixtures/info.json"));
        assert_eq!(info.contract, CONTRACT);
        assert!(info.rule("acestream").unwrap().can(Capability::Check));
        round_trip::<MatchRequest>(include_str!("../fixtures/match_request.json"));
        let m: MatchResponse = round_trip(include_str!("../fixtures/match_response.json"));
        assert!(matches!(&m.results[0], MatchResult::Matched(x) if x.reference.starts_with("acestream://")));
        assert!(matches!(&m.results[2], MatchResult::NoMatch { .. }));
        round_trip::<ObserveRequest>(include_str!("../fixtures/observe_request.json"));
        round_trip::<ObserveResponse>(include_str!("../fixtures/observe_response.json"));
        round_trip::<CheckRequest>(include_str!("../fixtures/check_request.json"));
        round_trip::<CheckResponse>(include_str!("../fixtures/check_response.json"));
        round_trip::<ErrorBody>(include_str!("../fixtures/error.json"));
    }

    #[test]
    fn unknown_capabilities_are_tolerated() {
        let r: RuleInfo = serde_json::from_str(
            r#"{"name":"x","title":"X","capabilities":["observe","teleport"],"match_revision":"1","play_revision":"1"}"#,
        )
        .unwrap();
        assert_eq!(r.capabilities, vec![Capability::Observe, Capability::Unknown]);
    }
}

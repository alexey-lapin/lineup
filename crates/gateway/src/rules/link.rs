//! The `link` kind: play URLs from a template, for streams behind another service (a proxy, an
//! engine, a CDN) keyed by what the rule's patterns capture. The gateway only matches; it does not
//! look anything up, observe or check.

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use gateway_contract::{CheckOutcome, CheckResponse, Hints, MatchResult, Matched, Observation, RuleInfo};

use crate::config::{LinkConfig, RuleConfig};
use crate::matcher::Matcher;
use crate::rules::Rule;

pub struct LinkRule {
    name: String,
    title: String,
    input_hint: Option<String>,
    group: Option<String>,
    config: RuleConfig,
    matcher: Matcher,
    revisions: (String, String),
}

impl LinkRule {
    pub fn new(name: &str, cfg: &LinkConfig) -> anyhow::Result<Self> {
        let config = RuleConfig::Link(cfg.clone());
        Ok(LinkRule {
            name: name.to_string(),
            title: cfg.title.clone().unwrap_or_else(|| name.to_string()),
            input_hint: cfg.input_hint.clone(),
            group: cfg.group.clone(),
            matcher: config.matcher(name)?,
            revisions: config.revisions(name),
            config,
        })
    }
}

impl Rule for LinkRule {
    fn info(&self) -> RuleInfo {
        RuleInfo {
            name: self.name.clone(),
            title: self.title.clone(),
            input_hint: self.input_hint.clone(),
            capabilities: vec![],
            match_revision: self.revisions.0.clone(),
            play_revision: self.revisions.1.clone(),
        }
    }

    fn match_inputs<'a>(&'a self, inputs: &'a [String]) -> BoxFuture<'a, Vec<MatchResult>> {
        let results = inputs
            .iter()
            .map(|input| {
                let no = |reason: String| MatchResult::NoMatch {
                    input: input.clone(),
                    reason: Some(reason),
                };
                let Some(m) = self.matcher.matches(input) else {
                    return no(format!("not a {} reference", self.title));
                };
                match self.config.play(&self.name, &m) {
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
                    Err(e) => no(e.to_string()),
                }
            })
            .collect();
        std::future::ready(results).boxed()
    }

    fn observe<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Observation> {
        std::future::ready(Observation {
            reference: reference.to_string(),
            available: false,
            reason: Some("link rules do not observe".into()),
            content_state: None,
            watching: None,
            facts: vec![],
            title: None,
            raw: None,
        })
        .boxed()
    }

    fn check<'a>(&'a self, reference: &'a str, _force: bool) -> BoxFuture<'a, CheckResponse> {
        std::future::ready(CheckResponse {
            reference: reference.to_string(),
            outcome: CheckOutcome::Skipped,
            reason: Some("unsupported".into()),
            content_state: None,
            detail: Some(serde_json::json!({ "reason": "link rules do not check" })),
            checked_at: chrono::Utc::now().to_rfc3339(),
        })
        .boxed()
    }
}

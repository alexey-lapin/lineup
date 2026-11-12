//! Client of the gateway contract (crate `gateway-contract`): the only outgoing calls lineup makes
//! for routes. Requests never follow redirects, answers are size-capped, and every failure maps to
//! a gateway error that names the gateway, so a broken gateway is never mistaken for bad input.

use std::time::Duration;

use gateway_contract::{
    CONTRACT, CheckRequest, CheckResponse, ErrorBody, Info, MAX_MATCH_INPUTS, MAX_OBSERVE_REFS, MatchRequest,
    MatchResponse, MatchResult, Observation, ObserveRequest, ObserveResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Deadlines per call. `match` may look up titles for hundreds of inputs; `check` runs a probe.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub info: Duration,
    pub matching: Duration,
    pub observe: Duration,
    pub check: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            info: Duration::from_secs(5),
            matching: Duration::from_secs(60),
            observe: Duration::from_secs(15),
            check: Duration::from_secs(120),
        }
    }
}

/// Largest gateway answer read.
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Gateway {
    client: reqwest::Client,
    timing: Timing,
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new(Timing::default())
    }
}

impl Gateway {
    pub fn new(timing: Timing) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building an HTTP client");
        Gateway { client, timing }
    }

    /// The gateway's description; fails unless it speaks this contract version.
    pub async fn info(&self, base: &str) -> Result<Info> {
        let info: Info = self.call(base, "info", None::<&()>, self.timing.info).await?;
        if info.contract != CONTRACT {
            return Err(Error::ContractMismatch(format!(
                "the gateway at {base} speaks contract {}, lineup needs {CONTRACT}",
                info.contract
            )));
        }
        Ok(info)
    }

    /// Match `inputs` with `rule`, in batches; one result per input, in order.
    pub async fn match_inputs(&self, base: &str, rule: &str, inputs: &[String]) -> Result<Vec<MatchResult>> {
        let mut out = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(MAX_MATCH_INPUTS) {
            let req = MatchRequest {
                rule: rule.to_string(),
                inputs: chunk.to_vec(),
            };
            let res: MatchResponse = self.call(base, "match", Some(&req), self.timing.matching).await?;
            if res.results.len() != chunk.len() {
                return Err(Error::GatewayError(format!(
                    "the gateway at {base} answered {} match results for {} inputs",
                    res.results.len(),
                    chunk.len()
                )));
            }
            for r in &res.results {
                if let MatchResult::Matched(m) = r {
                    check_matched(base, m)?;
                }
            }
            out.extend(res.results);
        }
        Ok(out)
    }

    /// Passive observation of `refs`; one result per ref, in order.
    pub async fn observe(&self, base: &str, rule: &str, refs: &[String]) -> Result<Vec<Observation>> {
        let mut out = Vec::with_capacity(refs.len());
        for chunk in refs.chunks(MAX_OBSERVE_REFS) {
            let req = ObserveRequest {
                rule: rule.to_string(),
                refs: chunk.to_vec(),
            };
            let res: ObserveResponse = self.call(base, "observe", Some(&req), self.timing.observe).await?;
            if res.results.len() != chunk.len() {
                return Err(Error::GatewayError(format!(
                    "the gateway at {base} answered {} observations for {} refs",
                    res.results.len(),
                    chunk.len()
                )));
            }
            out.extend(res.results);
        }
        Ok(out)
    }

    /// Active check of one ref.
    pub async fn check(&self, base: &str, rule: &str, reference: &str, force: bool) -> Result<CheckResponse> {
        let req = CheckRequest {
            rule: rule.to_string(),
            reference: reference.to_string(),
            force,
        };
        self.call(base, "check", Some(&req), self.timing.check).await
    }

    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        base: &str,
        op: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T> {
        let url = format!("{}/v1/{op}", base.trim_end_matches('/'));
        let req = match body {
            Some(b) => self.client.post(&url).json(b),
            None => self.client.get(&url),
        };
        let work = async {
            let mut res = req.send().await.map_err(|e| unreachable(base, e))?;
            let status = res.status();
            let mut bytes = Vec::new();
            while let Some(chunk) = res.chunk().await.map_err(|e| unreachable(base, e))? {
                if bytes.len() + chunk.len() > MAX_BODY {
                    return Err(Error::GatewayError(format!(
                        "the gateway at {base} sent more than {MAX_BODY} bytes for {op}"
                    )));
                }
                bytes.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                let msg = serde_json::from_slice::<ErrorBody>(&bytes)
                    .map(|b| format!("{} ({})", b.error.message, b.error.code))
                    .unwrap_or_else(|_| "no error details".into());
                return Err(Error::GatewayError(format!(
                    "the gateway at {base} answered {op} with HTTP {}: {msg}",
                    status.as_u16()
                )));
            }
            serde_json::from_slice::<T>(&bytes).map_err(|e| {
                Error::GatewayError(format!(
                    "the gateway at {base} sent a {op} answer lineup does not understand: {e}"
                ))
            })
        };
        tokio::time::timeout(timeout, work).await.unwrap_or_else(|_| {
            Err(Error::GatewayUnreachable(format!(
                "the gateway at {base} did not answer {op} within {}s",
                timeout.as_secs()
            )))
        })
    }
}

fn unreachable(base: &str, e: reqwest::Error) -> Error {
    Error::GatewayUnreachable(format!("the gateway at {base} is unreachable: {}", e.without_url()))
}

/// A matched result must carry a usable identity, ref and play reference.
fn check_matched(base: &str, m: &gateway_contract::Matched) -> Result<()> {
    let bad = |what: &str| {
        Err(Error::GatewayError(format!(
            "the gateway at {base} matched {:?} with {what}",
            m.input
        )))
    };
    if m.identity.trim().is_empty() {
        return bad("an empty identity");
    }
    if m.reference.trim().is_empty() {
        return bad("an empty ref");
    }
    if !valid_play(&m.play) {
        return bad("a play reference that is neither a relative path nor an http(s) URL");
    }
    Ok(())
}

/// A play reference is an absolute http(s) URL or a relative path without a scheme.
fn valid_play(play: &str) -> bool {
    let p = play.trim();
    if p.is_empty() || p.chars().any(char::is_whitespace) {
        return false;
    }
    match url::Url::parse(p) {
        Ok(u) => matches!(u.scheme(), "http" | "https") && u.host_str().is_some(),
        Err(url::ParseError::RelativeUrlWithoutBase) => !p.starts_with("//"),
        Err(_) => false,
    }
}

/// The URL players get for a play reference: an absolute URL as it is, a relative one appended
/// below the gateway base path (so a reverse-proxy prefix such as `/gateway` is kept).
pub fn play_url(base: &str, play: &str) -> String {
    if play.starts_with("http://") || play.starts_with("https://") {
        return play.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), play.trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_below_the_base_path() {
        assert_eq!(
            play_url("http://h:8099", "play/vk.m3u8?ref=x"),
            "http://h:8099/play/vk.m3u8?ref=x"
        );
        assert_eq!(
            play_url("http://proxy/gateway/", "play/vk.m3u8?ref=x"),
            "http://proxy/gateway/play/vk.m3u8?ref=x"
        );
        assert_eq!(
            play_url("http://proxy/gateway", "/play/vk.m3u8"),
            "http://proxy/gateway/play/vk.m3u8",
            "a stray leading slash does not drop the prefix"
        );
        assert_eq!(
            play_url("http://proxy/gateway", "http://acexy:8080/ace/getstream?id=1"),
            "http://acexy:8080/ace/getstream?id=1"
        );
    }

    #[test]
    fn play_references() {
        assert!(valid_play("play/vk.m3u8?ref=a%20b"));
        assert!(valid_play("http://acexy:8080/ace/getstream?id=1"));
        assert!(!valid_play(""));
        assert!(!valid_play("javascript:alert(1)"));
        assert!(!valid_play("//evil/x"));
        assert!(!valid_play("ftp://x/y"));
        assert!(!valid_play("play/a b"));
    }
}

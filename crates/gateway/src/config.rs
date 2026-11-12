//! Gateway configuration: a TOML file, with `LINEUP_GATEWAY_LISTEN` overriding the listen address.
//!
//! ```toml
//! listen = "0.0.0.0:8099"
//!
//! [acestream]                                  # the acestream rule; omit to turn it off
//! engine_control = "http://acestream:6878"     # gateway → engine (names, checks)
//! acexy_control  = "http://acexy:8080"         # gateway → acexy (activity guard, observe)
//! acexy_public   = "http://192.168.1.50:8080"  # written into play URLs for players
//!
//! [vk]                                         # the vk rule; omit to turn it off
//!
//! [tools]
//! yt_dlp = "/opt/tools/bin/yt-dlp"
//! streamlink = "/opt/tools/bin/streamlink"     # optional fallback
//!
//! [egress]
//! allow = []   # hosts public fetches may reach although they are private (tests only)
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default)]
    pub acestream: Option<AcestreamConfig>,
    #[serde(default)]
    pub vk: Option<VkConfig>,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub egress: EgressConfig,
    #[serde(default)]
    pub limits: Limits,
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8099".parse().unwrap()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcestreamConfig {
    pub engine_control: String,
    pub acexy_control: Option<String>,
    pub acexy_public: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VkConfig {}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    pub yt_dlp: Option<PathBuf>,
    pub streamlink: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    /// Hosts (names or IP literals) that public fetches may reach even though they are or resolve
    /// to private addresses. Empty in production; tests use it for local fake upstreams.
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Extractions running at once.
    pub extractions: usize,
    /// Extraction deadline, seconds.
    pub extraction_timeout_secs: u64,
    /// Largest playlist read, bytes.
    pub manifest_bytes: usize,
    /// Relay sessions kept at once.
    pub sessions: usize,
    /// A session without requests for this long is dropped, seconds.
    pub session_idle_secs: u64,
    /// Upstream resources one session may map.
    pub session_resources: usize,
    /// Upstream request deadline (headers), seconds.
    pub upstream_timeout_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            extractions: 2,
            extraction_timeout_secs: 60,
            manifest_bytes: 2 * 1024 * 1024,
            sessions: 64,
            session_idle_secs: 1800,
            session_resources: 100_000,
            upstream_timeout_secs: 15,
        }
    }
}

impl Limits {
    pub fn extraction_timeout(&self) -> Duration {
        Duration::from_secs(self.extraction_timeout_secs)
    }
    pub fn session_idle(&self) -> Duration {
        Duration::from_secs(self.session_idle_secs)
    }
    pub fn upstream_timeout(&self) -> Duration {
        Duration::from_secs(self.upstream_timeout_secs)
    }
}

impl Config {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut c: Config = toml::from_str(text)?;
        if let Ok(l) = std::env::var("LINEUP_GATEWAY_LISTEN") {
            c.listen = l.parse()?;
        }
        if let Some(a) = &mut c.acestream {
            a.engine_control = base(&a.engine_control, "acestream.engine_control")?;
            a.acexy_public = base(&a.acexy_public, "acestream.acexy_public")?;
            if let Some(x) = &a.acexy_control {
                a.acexy_control = Some(base(x, "acestream.acexy_control")?);
            }
        }
        if c.vk.is_some() && c.tools.yt_dlp.is_none() && c.tools.streamlink.is_none() {
            anyhow::bail!("the vk rule needs tools.yt_dlp or tools.streamlink");
        }
        Ok(c)
    }
}

/// An http(s) base URL without query, fragment or trailing slash.
fn base(v: &str, name: &str) -> anyhow::Result<String> {
    let v = v.trim().trim_end_matches('/');
    let u = url::Url::parse(v).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || u.query().is_some() {
        anyhow::bail!("{name}: must be an http(s) URL with a host and no query");
    }
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        let c = Config::parse(
            r#"
            listen = "127.0.0.1:1"
            [acestream]
            engine_control = "http://e:6878/"
            acexy_public = "http://a:8080/"
            [vk]
            [tools]
            yt_dlp = "/x/yt-dlp"
            "#,
        )
        .unwrap();
        let a = c.acestream.unwrap();
        assert_eq!(a.engine_control, "http://e:6878");
        assert_eq!(a.acexy_public, "http://a:8080");
        assert!(Config::parse("[vk]\n").is_err(), "vk needs a tool");
        assert!(Config::parse("bogus = 1\n").is_err());
        assert!(Config::parse("[acestream]\nengine_control='x'\nacexy_public='http://a'\n").is_err());
    }
}

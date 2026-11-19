//! Gateway configuration: a TOML file, with `LINEUP_GATEWAY_LISTEN` overriding the listen address.
//!
//! Rules are named blocks with a `kind`; extractors are named commands that relay rules use. See
//! `deploy/gateway.example.toml` for a complete example.
//!
//! ```toml
//! listen = "0.0.0.0:8099"
//!
//! [extractors.yt-dlp]
//! command = "/opt/tools/bin/yt-dlp"
//! args = ["--ignore-config", "--dump-single-json", "--proxy", "{proxy}", "--", "{url}"]
//! output = "yt-dlp-json"
//!
//! [rules.pages]
//! kind = "vk"
//! extractors = ["yt-dlp"]
//!
//! [rules.streams]
//! kind = "acestream"
//! engine_control = "http://engine:6878"
//! acexy_control = "http://acexy:8080"
//! acexy_public = "http://media-host:8080"
//! ```

use std::collections::BTreeMap;
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
    pub rules: BTreeMap<String, RuleConfig>,
    #[serde(default)]
    pub extractors: BTreeMap<String, ExtractorConfig>,
    #[serde(default)]
    pub egress: EgressConfig,
    #[serde(default)]
    pub limits: Limits,
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8099".parse().unwrap()
}

/// One rule: a name (the key) and a kind with its settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleConfig {
    Acestream(AcestreamConfig),
    Vk(PageConfig),
}

impl RuleConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            RuleConfig::Acestream(_) => "acestream",
            RuleConfig::Vk(_) => "vk",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcestreamConfig {
    /// Shown in lineup; defaults to the rule's name.
    pub title: Option<String>,
    pub input_hint: Option<String>,
    pub engine_control: String,
    pub acexy_control: Option<String>,
    pub acexy_public: String,
}

/// A rule whose pages are looked up by extractors and relayed as HLS.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageConfig {
    pub title: Option<String>,
    pub input_hint: Option<String>,
    /// Group suggested for matched channels.
    pub group: Option<String>,
    /// Extractor names, tried in order.
    pub extractors: Vec<String>,
    /// Upstream statuses that mean the signed URLs expired.
    #[serde(default = "default_expired_status")]
    pub expired_status: Vec<u16>,
    /// Query parameter of a manifest URL that carries its expiry (Unix time).
    #[serde(default = "default_expires_param")]
    pub expires_param: String,
}

fn default_expired_status() -> Vec<u16> {
    vec![400, 401, 403, 404, 410]
}

fn default_expires_param() -> String {
    "expires".into()
}

/// An extractor: a command that looks a page up and prints what to play.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorConfig {
    pub command: PathBuf,
    /// Arguments; `{url}` is the page, `{proxy}` the egress proxy URL (both required).
    pub args: Vec<String>,
    pub output: OutputFormat,
    /// Run deadline, seconds; defaults to `limits.extraction_timeout_secs`.
    pub timeout_secs: Option<u64>,
    /// Arguments that print the version at startup (proves the command runs).
    #[serde(default = "default_version_args")]
    pub version_args: Vec<String>,
    /// Extra phrases in the tool's error output meaning the page has no video.
    #[serde(default)]
    pub not_found: Vec<String>,
    /// Extra phrases meaning the page has a video but no playable stream yet.
    #[serde(default)]
    pub no_stream: Vec<String>,
}

fn default_version_args() -> Vec<String> {
    vec!["--version".into()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// `yt-dlp --dump-single-json`.
    YtDlpJson,
    /// `streamlink --json URL best`.
    StreamlinkJson,
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

    fn validate(&self) -> anyhow::Result<()> {
        let positive = [
            ("extractions", self.extractions as u64),
            ("extraction_timeout_secs", self.extraction_timeout_secs),
            ("manifest_bytes", self.manifest_bytes as u64),
            ("sessions", self.sessions as u64),
            ("session_idle_secs", self.session_idle_secs),
            ("session_resources", self.session_resources as u64),
            ("upstream_timeout_secs", self.upstream_timeout_secs),
        ];
        for (name, v) in positive {
            if v == 0 {
                anyhow::bail!("limits.{name} must be more than 0");
            }
        }
        if self.extraction_timeout_secs > 3600 || self.upstream_timeout_secs > 3600 {
            anyhow::bail!("limits: timeouts are at most 3600 seconds");
        }
        Ok(())
    }
}

/// Sections of the 0.5 format, refused with a pointer to the migration notes.
const OLD_SECTIONS: [&str; 3] = ["acestream", "vk", "tools"];

impl Config {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        if let Ok(table) = text.parse::<toml::Table>()
            && let Some(old) = OLD_SECTIONS.iter().find(|s| table.contains_key(**s))
        {
            anyhow::bail!(
                "[{old}] is the configuration format of lineup-gateway 0.5; rules are now [rules.<name>] \
                 blocks with a kind, and tools are [extractors.<name>] (see \"Migrating from 0.5\" in \
                 docs/gateway.md)"
            );
        }
        let mut c: Config = toml::from_str(text)?;
        if let Ok(l) = std::env::var("LINEUP_GATEWAY_LISTEN") {
            c.listen = l.parse()?;
        }
        c.limits.validate()?;
        for (name, x) in &c.extractors {
            if !valid_name(name) {
                anyhow::bail!("extractors.{name}: names are 1-32 of a-z, 0-9, '-' and '_'");
            }
            for p in ["{url}", "{proxy}"] {
                if !x.args.iter().any(|a| a.contains(p)) {
                    anyhow::bail!("extractors.{name}: args must contain {p}");
                }
            }
            if x.timeout_secs.is_some_and(|t| t == 0 || t > 3600) {
                anyhow::bail!("extractors.{name}: timeout_secs must be 1-3600");
            }
        }
        for (name, rule) in &mut c.rules {
            if !valid_name(name) {
                anyhow::bail!("rules.{name}: names are 1-32 of a-z, 0-9, '-' and '_', starting with a letter or digit");
            }
            match rule {
                RuleConfig::Acestream(a) => {
                    a.engine_control = base(&a.engine_control, &format!("rules.{name}.engine_control"))?;
                    a.acexy_public = base(&a.acexy_public, &format!("rules.{name}.acexy_public"))?;
                    if let Some(x) = &a.acexy_control {
                        a.acexy_control = Some(base(x, &format!("rules.{name}.acexy_control"))?);
                    }
                }
                RuleConfig::Vk(p) => {
                    if p.extractors.is_empty() {
                        anyhow::bail!("rules.{name}: extractors must name at least one extractor");
                    }
                    for x in &p.extractors {
                        if !c.extractors.contains_key(x) {
                            anyhow::bail!("rules.{name}: no extractor named {x:?}");
                        }
                    }
                    if p.expired_status.iter().any(|s| !(400..600).contains(s)) {
                        anyhow::bail!("rules.{name}.expired_status: only 4xx and 5xx statuses");
                    }
                    if p.expires_param.is_empty() {
                        anyhow::bail!("rules.{name}.expires_param must not be empty");
                    }
                }
            }
        }
        Ok(c)
    }
}

/// Rule and extractor names: they appear in URLs and in lineup's routes.
fn valid_name(n: &str) -> bool {
    (1..=32).contains(&n.len())
        && n.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
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

    const X: &str = r#"
        [extractors.yt-dlp]
        command = "/x/yt-dlp"
        args = ["--proxy", "{proxy}", "--", "{url}"]
        output = "yt-dlp-json"
    "#;

    #[test]
    fn parses_and_normalises() {
        let c = Config::parse(&format!(
            r#"
            listen = "127.0.0.1:1"
            [rules.ace]
            kind = "acestream"
            engine_control = "http://e:6878/"
            acexy_public = "http://a:8080/"
            [rules.pages]
            kind = "vk"
            extractors = ["yt-dlp"]
            {X}
            "#
        ))
        .unwrap();
        let RuleConfig::Acestream(a) = &c.rules["ace"] else {
            panic!("kind")
        };
        assert_eq!(a.engine_control, "http://e:6878");
        assert_eq!(a.acexy_public, "http://a:8080");
        let RuleConfig::Vk(p) = &c.rules["pages"] else {
            panic!("kind")
        };
        assert_eq!(p.expired_status, [400, 401, 403, 404, 410]);
        assert_eq!(c.extractors["yt-dlp"].output, OutputFormat::YtDlpJson);
    }

    #[test]
    fn refuses_bad_configs() {
        let bad = [
            "bogus = 1\n".to_string(),
            "[vk]\n".into(),
            "[tools]\nyt_dlp = '/x'\n".into(),
            "[rules.a]\nkind = 'nope'\n".into(),
            "[rules.a]\nkind = 'acestream'\nengine_control='x'\nacexy_public='http://a'\n".into(),
            "[rules.a]\nkind = 'acestream'\nengine_control='http://e'\nacexy_public='http://a'\nextra = 1\n".into(),
            "[rules.A]\nkind = 'vk'\nextractors = ['yt-dlp']\n".to_string() + X,
            "[rules.a]\nkind = 'vk'\nextractors = []\n".to_string() + X,
            "[rules.a]\nkind = 'vk'\nextractors = ['other']\n".to_string() + X,
            "[rules.a]\nkind = 'vk'\nextractors = ['yt-dlp']\nexpired_status = [200]\n".to_string() + X,
            "[extractors.x]\ncommand = '/x'\nargs = ['{url}']\noutput = 'yt-dlp-json'\n".into(),
            "[extractors.x]\ncommand = '/x'\nargs = ['{url}', '{proxy}']\noutput = 'other'\n".into(),
            "[limits]\nextractions = 0\nextraction_timeout_secs = 1\nmanifest_bytes = 1\nsessions = 1\n\
             session_idle_secs = 1\nsession_resources = 1\nupstream_timeout_secs = 1\n"
                .into(),
        ];
        for b in &bad {
            assert!(Config::parse(b).is_err(), "{b}");
        }
        let old = Config::parse("[vk]\n").unwrap_err().to_string();
        assert!(old.contains("Migrating from 0.5"), "{old}");
    }

    #[test]
    fn the_example_config_parses() {
        let text = include_str!("../../../deploy/gateway.example.toml");
        let c = Config::parse(text).unwrap();
        assert!(c.rules.contains_key("vk"));
        // Its commented-out rules parse too.
        let mut uncommented = String::new();
        let mut block = false;
        for line in text.lines() {
            block = line.starts_with("# [rules.") || (block && line.starts_with("# "));
            uncommented.push_str(if block { &line[2..] } else { line });
            uncommented.push('\n');
        }
        let c = Config::parse(&uncommented).unwrap();
        assert!(c.rules.contains_key("acestream"));
    }
}

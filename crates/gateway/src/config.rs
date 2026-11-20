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
//! kind = "relay"
//! extractors = ["yt-dlp"]
//! identity = "page:{id}"
//! ref = "https://video.example/v/{id}"
//! [[rules.pages.match]]
//! hosts = ["video.example"]
//! path = '^/v/(?P<id>[0-9]+)'
//!
//! [rules.streams]
//! kind = "acestream"
//! engine_control = "http://engine:6878"
//! acexy_control = "http://acexy:8080"
//! play = "http://media-host:8080/ace/getstream?id={id}"
//! identity = "ace:{id}"
//! ref = "acestream://{id}"
//! [[rules.streams.match]]
//! input = '(?i)^(?P<id>[0-9a-f]{40})$'
//! ```

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::matcher::{ExampleConfig, Match, Matcher, PatternConfig, VarType};
use crate::template::{Template, Use};

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
    Relay(RelayConfig),
    Link(LinkConfig),
}

/// How a rule recognises inputs: patterns, typed variables, identity and ref templates, and the
/// examples checked on load. Shared by every kind.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    #[serde(rename = "match")]
    pub patterns: Vec<PatternConfig>,
    #[serde(default)]
    pub vars: BTreeMap<String, VarType>,
    pub identity: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

impl RuleConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            RuleConfig::Acestream(_) => "acestream",
            RuleConfig::Relay(_) => "relay",
            RuleConfig::Link(_) => "link",
        }
    }

    pub fn spec(&self) -> MatchSpec {
        let (patterns, vars, identity, reference) = match self {
            RuleConfig::Acestream(a) => (&a.patterns, &a.vars, &a.identity, &a.reference),
            RuleConfig::Relay(r) => (&r.patterns, &r.vars, &r.identity, &r.reference),
            RuleConfig::Link(l) => (&l.patterns, &l.vars, &l.identity, &l.reference),
        };
        MatchSpec {
            patterns: patterns.clone(),
            vars: vars.clone(),
            identity: identity.clone(),
            reference: reference.clone(),
        }
    }

    pub fn examples(&self) -> &[ExampleConfig] {
        match self {
            RuleConfig::Acestream(a) => &a.examples,
            RuleConfig::Relay(r) => &r.examples,
            RuleConfig::Link(l) => &l.examples,
        }
    }

    /// The rule's matcher. Relay rules also accept their own play URLs (`play/<name>.m3u8`).
    pub fn matcher(&self, name: &str) -> anyhow::Result<Matcher> {
        let s = self.spec();
        let own = matches!(self, RuleConfig::Relay(_)).then(|| relay_play_path(name));
        let play = match self {
            RuleConfig::Acestream(a) => Some(a.play.as_str()),
            RuleConfig::Link(l) => Some(l.play.as_str()),
            RuleConfig::Relay(_) => None,
        };
        Matcher::new(name, &s.patterns, &s.vars, &s.identity, &s.reference, play, own)
    }

    /// The play reference for a match: the acestream `play` template, or the relay path.
    pub fn play(&self, name: &str, m: &Match) -> anyhow::Result<String> {
        Ok(match self {
            RuleConfig::Acestream(a) => Template::parse(&a.play, Use::Play)?.render(&m.vars),
            RuleConfig::Link(l) => Template::parse(&l.play, Use::Play)?.render(&m.vars),
            RuleConfig::Relay(_) => format!(
                "{}?ref={}",
                relay_play_path(name),
                crate::template::encode(&m.reference)
            ),
        })
    }

    /// `match_revision` and `play_revision` (see `revision`).
    pub fn revisions(&self, name: &str) -> (String, String) {
        use crate::revision::{match_revision, play_revision};
        match self {
            RuleConfig::Acestream(a) => {
                // The engine's names, cleaned by code, are hints: version 1 of that cleaning.
                let m = match_revision("acestream", 1, &(name, self.spec()));
                let p = play_revision("acestream", &a.play, &m);
                (m, p)
            }
            RuleConfig::Relay(r) => {
                let m = match_revision("relay", 0, &(name, self.spec(), &r.title, &r.group));
                let p = play_revision("relay", &relay_play_path(name), &m);
                (m, p)
            }
            RuleConfig::Link(l) => {
                let m = match_revision("link", 0, &(name, self.spec(), &l.title, &l.group));
                let p = play_revision("link", &l.play, &m);
                (m, p)
            }
        }
    }

    /// Compile the matcher and the play template, and check every example: its expected
    /// identity, ref and play, and that its ref is accepted again with the same identity and ref.
    pub fn validate(&self, name: &str) -> anyhow::Result<()> {
        let m = self.matcher(name)?;
        if let RuleConfig::Acestream(a) = self {
            if !m.names().contains("id") {
                anyhow::bail!("an acestream rule's patterns must capture id");
            }
            if a.checks && a.acexy_control.is_none() {
                anyhow::bail!("checks need acexy_control: only acexy tells whether a stream is watched");
            }
        }
        for (i, ex) in self.examples().iter().enumerate() {
            let at = format!("examples[{i}] ({:?})", ex.input);
            let got = m.matches(&ex.input);
            let Some(got) = got else {
                if ex.matches {
                    anyhow::bail!("{at}: not accepted");
                }
                continue;
            };
            if !ex.matches {
                anyhow::bail!("{at}: accepted as {}, expected no match", got.identity);
            }
            let play = self.play(name, &got)?;
            for (what, want, have) in [
                ("identity", &ex.identity, &got.identity),
                ("ref", &ex.reference, &got.reference),
                ("play", &ex.play, &play),
            ] {
                if let Some(w) = want
                    && w != have
                {
                    anyhow::bail!("{at}: {what} is {have:?}, expected {w:?}");
                }
            }
            match m.matches(&got.reference) {
                Some(again) if again.identity == got.identity && again.reference == got.reference => {}
                other => anyhow::bail!(
                    "{at}: its ref {:?} does not match again to the same identity and ref (got {:?})",
                    got.reference,
                    other.map(|o| (o.identity, o.reference))
                ),
            }
        }
        Ok(())
    }
}

/// A relay rule's play path, relative to the gateway.
pub fn relay_play_path(name: &str) -> String {
    format!("play/{name}.m3u8")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcestreamConfig {
    /// Shown in lineup; defaults to the rule's name.
    pub title: Option<String>,
    pub input_hint: Option<String>,
    pub engine_control: String,
    pub acexy_control: Option<String>,
    /// Play URL template, as players reach the stream (acexy, the engine or any proxy).
    pub play: String,
    /// Guarded checks: a short engine session, skipped while acexy reports viewers. Needs
    /// `acexy_control`, and players that play through acexy (the guard sees no one else).
    #[serde(default)]
    pub checks: bool,
    #[serde(rename = "match")]
    pub patterns: Vec<PatternConfig>,
    #[serde(default)]
    pub vars: BTreeMap<String, VarType>,
    pub identity: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub examples: Vec<ExampleConfig>,
}

/// A rule whose play URLs are a template: streams behind some other service, keyed by what the
/// patterns capture. No lookups, observation or checks.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub title: Option<String>,
    pub input_hint: Option<String>,
    pub group: Option<String>,
    pub play: String,
    #[serde(rename = "match")]
    pub patterns: Vec<PatternConfig>,
    #[serde(default)]
    pub vars: BTreeMap<String, VarType>,
    pub identity: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub examples: Vec<ExampleConfig>,
}

/// A rule whose pages are looked up by extractors and relayed as HLS.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayConfig {
    pub title: Option<String>,
    pub input_hint: Option<String>,
    #[serde(rename = "match")]
    pub patterns: Vec<PatternConfig>,
    #[serde(default)]
    pub vars: BTreeMap<String, VarType>,
    pub identity: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub examples: Vec<ExampleConfig>,
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
    /// Extra environment variables (the environment is otherwise cleared but for PATH, HOME,
    /// LANG, LC_ALL, LC_CTYPE, TZ, TMPDIR). Proxy variables are always the egress proxy.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
    /// The plugin format: `{"manifest_url", "headers", "is_live", "title", "duration",
    /// "expires"}`, or `{"error", "kind"}` (see docs/gateway.md).
    DescriptorJson,
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
            for k in x.env.keys() {
                let reserved = crate::extract::PROXY_ENV
                    .iter()
                    .chain(crate::extract::BYPASS_ENV.iter())
                    .any(|r| r.eq_ignore_ascii_case(k));
                if reserved {
                    anyhow::bail!("extractors.{name}.env: {k} is always the gateway's egress proxy setting");
                }
                if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    anyhow::bail!("extractors.{name}.env: {k:?} is not a variable name");
                }
            }
        }
        for (name, rule) in &mut c.rules {
            if !valid_name(name) {
                anyhow::bail!("rules.{name}: names are 1-32 of a-z, 0-9, '-' and '_', starting with a letter or digit");
            }
            match rule {
                RuleConfig::Link(_) => {}
                RuleConfig::Acestream(a) => {
                    a.engine_control = base(&a.engine_control, &format!("rules.{name}.engine_control"))?;
                    if let Some(x) = &a.acexy_control {
                        a.acexy_control = Some(base(x, &format!("rules.{name}.acexy_control"))?);
                    }
                }
                RuleConfig::Relay(p) => {
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
            rule.validate(name).map_err(|e| anyhow::anyhow!("rules.{name}: {e}"))?;
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

    /// A relay rule `a` with one pattern, plus `extra` lines in its block.
    fn relay(extra: &str) -> String {
        format!(
            "[rules.a]\nkind = 'relay'\nextractors = ['yt-dlp']\nidentity = 'p:{{n}}'\n\
             ref = 'https://v.example/{{n}}'\n{extra}\n[[rules.a.match]]\nhosts = ['v.example']\n\
             path = '^/(?P<n>[0-9]+)$'\n{X}"
        )
    }

    const ACE: &str = "[rules.ace]\nkind = 'acestream'\nengine_control = 'http://e:6878/'\n\
        play = 'http://a:8080/ace/getstream?id={id}'\nidentity = 'ace:{id}'\nref = 'acestream://{id}'\n\
        [[rules.ace.match]]\ninput = '^(?P<id>[0-9a-f]{40})$'\n";

    #[test]
    fn parses_and_normalises() {
        let c = Config::parse(&format!("listen = '127.0.0.1:1'\n{ACE}{}", relay(""))).unwrap();
        let RuleConfig::Acestream(a) = &c.rules["ace"] else {
            panic!("kind")
        };
        assert_eq!(a.engine_control, "http://e:6878");
        let RuleConfig::Relay(p) = &c.rules["a"] else {
            panic!("kind")
        };
        assert_eq!(p.expired_status, [400, 401, 403, 404, 410]);
        assert_eq!(c.extractors["yt-dlp"].output, OutputFormat::YtDlpJson);
        let (m, play) = c.rules["a"].revisions("a");
        assert_ne!(m, play);
    }

    #[test]
    fn link_rules_and_extractor_env() {
        let link = ACE
            .replace("kind = 'acestream'", "kind = 'link'\ngroup = 'G'")
            .replace("engine_control = 'http://e:6878/'\n", "");
        let c = Config::parse(&format!("{link}{X}env = {{ XDG_CACHE_HOME = '/c' }}\n")).unwrap();
        assert_eq!(c.rules["ace"].kind(), "link");
        assert_eq!(c.extractors["yt-dlp"].env["XDG_CACHE_HOME"], "/c");
        let checks = ACE.replace(
            "kind = 'acestream'",
            "kind = 'acestream'\nchecks = true\nacexy_control = 'http://x'",
        );
        Config::parse(&checks).unwrap();
    }

    #[test]
    fn revisions_follow_the_config() {
        let rev = |extra: &str| {
            let c = Config::parse(&relay(extra)).unwrap();
            c.rules["a"].revisions("a")
        };
        let base = rev("");
        assert_eq!(base, rev(""), "stable");
        assert_eq!(base, rev("expires_param = 'e'"), "relay policy is not part of either");
        let group = rev("group = 'G'");
        assert_ne!(base.0, group.0, "hints are part of the match revision");
        assert_ne!(base.1, group.1, "and so of the play revision");
    }

    #[test]
    fn checks_examples() {
        let ok = [
            "[[rules.a.examples]]\ninput = 'https://v.example/1'\nidentity = 'p:1'\nref = 'https://v.example/1'\n\
             play = 'play/a.m3u8?ref=https%3A%2F%2Fv.example%2F1'",
            "[[rules.a.examples]]\ninput = 'https://v.example/x'\nmatch = false",
        ];
        for e in ok {
            Config::parse(&relay(e)).unwrap();
        }
        let bad = [
            "[[rules.a.examples]]\ninput = 'https://v.example/1'\nidentity = 'p:2'",
            "[[rules.a.examples]]\ninput = 'https://v.example/x'",
            "[[rules.a.examples]]\ninput = 'https://v.example/1'\nmatch = false",
        ];
        for e in bad {
            assert!(Config::parse(&relay(e)).is_err(), "{e}");
        }
        // A ref that its own patterns do not accept again is refused.
        let lossy = relay("").replace("ref = 'https://v.example/{n}'", "ref = 'https://v.example/x{n}'")
            + "[[rules.a.examples]]\ninput = 'https://v.example/1'\n";
        let e = Config::parse(&lossy).unwrap_err().to_string();
        assert!(e.contains("does not match again"), "{e}");
    }

    #[test]
    fn refuses_bad_configs() {
        let bad = [
            "bogus = 1\n".to_string(),
            "[vk]\n".into(),
            "[tools]\nyt_dlp = '/x'\n".into(),
            "[rules.a]\nkind = 'nope'\n".into(),
            ACE.replace("http://e:6878/", "x"),
            ACE.replace("kind = 'acestream'", "kind = 'acestream'\nextra = 1"),
            ACE.replace("{id}'\nidentity", "{other}'\nidentity"),
            ACE.replace("(?P<id>", "(?P<other>").replace("{id}", "{other}"),
            ACE.replace("play = 'http://a:8080", "play = 'http://{id}"),
            relay("").replace("[rules.a]", "[rules.A]"),
            relay("").replace("['yt-dlp']", "[]"),
            relay("").replace("['yt-dlp']", "['other']"),
            relay("expired_status = [200]"),
            relay("vars = { m = 'int' }"),
            ACE.replace("kind = 'acestream'", "kind = 'acestream'\nchecks = true"),
            format!("{X}env = {{ HTTPS_PROXY = 'x' }}\n"),
            format!("{X}env = {{ no_proxy = 'x' }}\n"),
            format!("{X}env = {{ 'A-B' = 'x' }}\n"),
            relay("").replace("ref = 'https://v.example/{n}'", "ref = 'https://{n}.example/'"),
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

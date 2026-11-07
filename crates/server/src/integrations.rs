//! Integrations: the services channels are played through, one per source kind. Their configs
//! decide each channel's playback URL and whether it is in playlist and guide output at all.

use api_types::{Availability, IntegrationConfig, IntegrationPatch, Source, SourceKind};

use crate::error::{Result, invalid};
use crate::source;

/// Upper bound for `probe_interval_hours` (one week).
pub const MAX_PROBE_INTERVAL_HOURS: u32 = 24 * 7;

/// The configs of all integrations, read together (one snapshot).
#[derive(Debug, Clone, PartialEq)]
pub struct Configs {
    pub acestream: AcestreamConfig,
    pub vk_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcestreamConfig {
    /// Blank when not configured.
    pub engine_url: String,
    pub acexy_url: Option<String>,
    pub probe_interval_hours: u32,
}

impl Default for Configs {
    fn default() -> Self {
        Configs::from_parts(SourceKind::ALL.map(IntegrationConfig::blank))
    }
}

impl Configs {
    /// From one config per kind (missing kinds stay blank).
    pub fn from_parts(parts: impl IntoIterator<Item = IntegrationConfig>) -> Self {
        let mut c = Configs {
            acestream: AcestreamConfig {
                engine_url: String::new(),
                acexy_url: None,
                probe_interval_hours: 0,
            },
            vk_url: String::new(),
        };
        for p in parts {
            c.set(p);
        }
        c
    }

    pub fn set(&mut self, config: IntegrationConfig) {
        match config {
            IntegrationConfig::Acestream {
                engine_url,
                acexy_url,
                probe_interval_hours,
            } => {
                self.acestream = AcestreamConfig {
                    engine_url,
                    acexy_url,
                    probe_interval_hours,
                }
            }
            IntegrationConfig::Vk { url } => self.vk_url = url,
            IntegrationConfig::Direct {} => {}
        }
    }

    pub fn get(&self, kind: SourceKind) -> IntegrationConfig {
        match kind {
            SourceKind::Acestream => IntegrationConfig::Acestream {
                engine_url: self.acestream.engine_url.clone(),
                acexy_url: self.acestream.acexy_url.clone(),
                probe_interval_hours: self.acestream.probe_interval_hours,
            },
            SourceKind::Vk => IntegrationConfig::Vk {
                url: self.vk_url.clone(),
            },
            SourceKind::Direct => IntegrationConfig::Direct {},
        }
    }

    pub fn configured(&self, kind: SourceKind) -> bool {
        self.get(kind).configured()
    }

    /// The base URL playback goes through, or None while the integration is not configured. Two
    /// configs with the same playback base produce the same playlist output.
    pub fn playback_base(&self, kind: SourceKind) -> Option<String> {
        match kind {
            SourceKind::Acestream => {
                let a = &self.acestream;
                if a.engine_url.is_empty() {
                    return None;
                }
                Some(a.acexy_url.clone().unwrap_or_else(|| a.engine_url.clone()))
            }
            SourceKind::Vk => Some(self.vk_url.clone()).filter(|u| !u.is_empty()),
            SourceKind::Direct => Some(String::new()),
        }
    }

    /// The URL emitted in playlists for `source`, or None while its integration is not configured.
    pub fn stream_url(&self, source: &Source) -> Option<String> {
        let base = self.playback_base(source.kind())?;
        Some(source::stream_url(source, &base))
    }

    /// Whether channel checks can run, and why not.
    pub fn checks(&self, kind: SourceKind) -> Availability {
        match kind {
            SourceKind::Acestream if self.acestream.engine_url.is_empty() => {
                Availability::no("the Acestream engine is not configured")
            }
            SourceKind::Acestream if self.acestream.acexy_url.is_none() => Availability::no(
                "checks need acexy: without it lineup cannot tell whether someone is watching a stream",
            ),
            SourceKind::Acestream => Availability::yes(),
            _ => Availability::no("this integration has no channel checks"),
        }
    }
}

/// Apply a patch to a config of the same kind, validating and normalising URLs.
pub fn apply(old: &IntegrationConfig, patch: IntegrationPatch) -> Result<IntegrationConfig> {
    match (old, patch) {
        (
            IntegrationConfig::Acestream {
                engine_url,
                acexy_url,
                probe_interval_hours,
            },
            IntegrationPatch::Acestream {
                engine_url: new_engine,
                acexy_url: new_acexy,
                probe_interval_hours: new_hours,
            },
        ) => {
            let engine_url = match new_engine {
                Some(v) => optional_base("engine_url", &v)?,
                None => engine_url.clone(),
            };
            let acexy_url = match new_acexy {
                Some(Some(v)) => Some(optional_base("acexy_url", &v)?).filter(|u| !u.is_empty()),
                Some(None) => None,
                None => acexy_url.clone(),
            };
            let probe_interval_hours = match new_hours {
                Some(h) if h > MAX_PROBE_INTERVAL_HOURS => {
                    return Err(invalid(format!(
                        "probe_interval_hours must be at most {MAX_PROBE_INTERVAL_HOURS}"
                    )));
                }
                Some(h) => h,
                None => *probe_interval_hours,
            };
            Ok(IntegrationConfig::Acestream {
                engine_url,
                acexy_url,
                probe_interval_hours,
            })
        }
        (IntegrationConfig::Vk { url }, IntegrationPatch::Vk { url: new }) => Ok(IntegrationConfig::Vk {
            url: match new {
                Some(v) => optional_base("url", &v)?,
                None => url.clone(),
            },
        }),
        (IntegrationConfig::Direct {}, IntegrationPatch::Direct {}) => Ok(IntegrationConfig::Direct {}),
        (old, patch) => Err(invalid(format!(
            "this is the {} integration, but the change is for {}",
            old.kind().as_str(),
            patch.kind().as_str()
        ))),
    }
}

/// A base URL, or "" for blank input (turns the integration or component off).
fn optional_base(name: &str, value: &str) -> Result<String> {
    if value.trim().is_empty() {
        return Ok(String::new());
    }
    source::normalize_base(name, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACE: &str = "41169a3f280a5537b0611b775e926852d372743d";

    fn ace(engine: &str, acexy: Option<&str>) -> Configs {
        Configs::from_parts([IntegrationConfig::Acestream {
            engine_url: engine.into(),
            acexy_url: acexy.map(Into::into),
            probe_interval_hours: 0,
        }])
    }

    #[test]
    fn acestream_urls() {
        let src = Source::Acestream { id: ACE.into() };
        assert_eq!(
            ace("", Some("http://acexy:8080")).stream_url(&src),
            None,
            "no engine: off"
        );
        assert_eq!(
            ace("http://e:6878", None).stream_url(&src).as_deref(),
            Some(format!("http://e:6878/ace/getstream?id={ACE}").as_str()),
            "no acexy: the engine directly"
        );
        assert_eq!(
            ace("http://e:6878", Some("http://a:8080")).stream_url(&src).as_deref(),
            Some(format!("http://a:8080/ace/getstream?id={ACE}").as_str())
        );
    }

    #[test]
    fn vk_and_direct() {
        let vk = Source::Vk {
            owner_id: -1,
            video_id: 2,
            list: None,
        };
        let direct = Source::Direct {
            url: "http://x/y.m3u8".into(),
        };
        let blank = Configs::default();
        assert_eq!(blank.stream_url(&vk), None);
        assert_eq!(blank.stream_url(&direct).as_deref(), Some("http://x/y.m3u8"));
        assert!(blank.configured(SourceKind::Direct));
        let mut c = blank.clone();
        c.set(IntegrationConfig::Vk {
            url: "http://v:8099".into(),
        });
        assert_eq!(c.stream_url(&vk).as_deref(), Some("http://v:8099/vk/-1_2"));
    }

    #[test]
    fn patches() {
        let old = IntegrationConfig::blank(SourceKind::Acestream);
        let p = |json: &str| serde_json::from_str::<IntegrationPatch>(json).unwrap();
        let new = apply(
            &old,
            p(r#"{"kind":"acestream","engine_url":" http://e:6878/ ","acexy_url":"http://a:8080"}"#),
        )
        .unwrap();
        assert_eq!(
            new,
            IntegrationConfig::Acestream {
                engine_url: "http://e:6878".into(),
                acexy_url: Some("http://a:8080".into()),
                probe_interval_hours: 0
            }
        );
        // Omitted keeps, null or "" clears.
        let kept = apply(&new, p(r#"{"kind":"acestream","probe_interval_hours":6}"#)).unwrap();
        assert!(matches!(
            &kept,
            IntegrationConfig::Acestream {
                acexy_url: Some(_),
                probe_interval_hours: 6,
                ..
            }
        ));
        for clear in [
            r#"{"kind":"acestream","acexy_url":null}"#,
            r#"{"kind":"acestream","acexy_url":""}"#,
        ] {
            let c = apply(&kept, p(clear)).unwrap();
            assert!(
                matches!(c, IntegrationConfig::Acestream { acexy_url: None, .. }),
                "{clear}"
            );
        }
        let off = apply(&kept, p(r#"{"kind":"acestream","engine_url":""}"#)).unwrap();
        assert!(!off.configured());
        assert!(apply(&old, p(r#"{"kind":"acestream","engine_url":"e:6878"}"#)).is_err());
        assert!(apply(&old, p(r#"{"kind":"acestream","probe_interval_hours":1000}"#)).is_err());
        assert!(
            apply(&old, p(r#"{"kind":"vk","url":"http://v"}"#)).is_err(),
            "kind mismatch"
        );
    }

    #[test]
    fn checks_need_acexy() {
        assert!(!ace("", None).checks(SourceKind::Acestream).available);
        assert!(!ace("http://e", None).checks(SourceKind::Acestream).available);
        assert!(
            ace("http://e", Some("http://a"))
                .checks(SourceKind::Acestream)
                .available
        );
        assert!(!ace("http://e", Some("http://a")).checks(SourceKind::Vk).available);
    }
}

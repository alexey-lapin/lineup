//! Wire types shared by the lineup server, CLI and (via OpenAPI) the web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProbeOutcome {
    /// The swarm has peers or is downloading.
    Ok,
    /// The session started but stayed in prebuffering with no peers.
    Dead,
    /// The engine or network failed; says nothing about the stream.
    Unknown,
    /// Not probed because the stream was being watched (or the guard could not tell).
    Skipped,
}

/// Probe results of a channel. Only `ok` and `dead` are conclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChannelHealth {
    /// Last conclusive result (`ok` or `dead`); drives the badge and events.
    pub conclusive: Option<ProbeOutcome>,
    pub conclusive_at: Option<DateTime<Utc>>,
    pub last_attempt: ProbeOutcome,
    pub last_attempt_at: DateTime<Utc>,
    /// Engine statistics or the error of the last attempt.
    #[schema(value_type = Object)]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ProbeRequest {
    /// Channels to check; all enabled acestream channels when empty (that run continues in the
    /// background). Checks need acexy.
    #[serde(default)]
    pub ids: Vec<i64>,
    /// Probe even if the stream seems to be in use.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProbeResult {
    pub channel_id: i64,
    pub outcome: ProbeOutcome,
    #[schema(value_type = Object)]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProbeResponse {
    /// True when a run over all channels was started in the background; results then arrive as
    /// channel updates and events.
    pub background: bool,
    /// Results, when specific channels were probed.
    pub results: Vec<ProbeResult>,
}

/// A channel's stream source. Its kind is also the channel's integration, which builds the URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// Acestream content id (40 lowercase hex chars), played through acexy (or the engine).
    Acestream { id: String },
    /// VK video, played through the VK proxy.
    Vk {
        owner_id: i64,
        video_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list: Option<String>,
    },
    /// Any other URL, emitted verbatim.
    Direct { url: String },
}

impl Source {
    pub fn kind(&self) -> SourceKind {
        match self {
            Source::Acestream { .. } => SourceKind::Acestream,
            Source::Vk { .. } => SourceKind::Vk,
            Source::Direct { .. } => SourceKind::Direct,
        }
    }
}

/// A source kind, which is also the integration that plays it (one integration per kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Acestream,
    Vk,
    Direct,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Acestream => "acestream",
            SourceKind::Vk => "vk",
            SourceKind::Direct => "direct",
        }
    }

    pub const ALL: [SourceKind; 3] = [SourceKind::Acestream, SourceKind::Vk, SourceKind::Direct];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "acestream" => Some(SourceKind::Acestream),
            "vk" => Some(SourceKind::Vk),
            "direct" => Some(SourceKind::Direct),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub source: Source,
    /// Canonical key used for de-duplication (unique per source kind).
    pub source_key: String,
    /// Stream URL as emitted in playlists; null while the channel's integration is not configured.
    pub stream_url: Option<String>,
    /// Whether the channel's integration is configured. Channels of an unconfigured integration
    /// stay listed but are left out of playlists and guides.
    pub configured: bool,
    pub is_event: bool,
    pub kickoff_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    /// Slugs of playlists containing this channel.
    pub playlists: Vec<String>,
    /// Swarm health from probes (acestream channels only); null until probed.
    pub health: Option<ChannelHealth>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChannelCreate {
    pub name: String,
    #[serde(default)]
    pub group_title: Option<String>,
    #[serde(default)]
    pub tvg_id: Option<String>,
    #[serde(default)]
    pub tvg_logo: Option<String>,
    /// Either a structured source, or free-form `input` (ace id, acestream:// link, vk.com URL, any URL).
    #[serde(default)]
    pub source: Option<Source>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub is_event: bool,
    #[serde(default)]
    pub kickoff_at: Option<DateTime<Utc>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Playlists (by slug) to append the new channel to.
    #[serde(default)]
    pub playlists: Vec<String>,
}

impl Default for ChannelCreate {
    /// Matches the wire defaults (notably `enabled: true`).
    fn default() -> Self {
        ChannelCreate {
            name: String::new(),
            group_title: None,
            tvg_id: None,
            tvg_logo: None,
            source: None,
            input: None,
            is_event: false,
            kickoff_at: None,
            enabled: true,
            playlists: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Partial update. Absent fields are left unchanged; for nullable fields, `null` clears.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ChannelPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub group_title: Option<Option<String>>,
    #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub tvg_id: Option<Option<String>>,
    #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub tvg_logo: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_event: Option<bool>,
    #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<DateTime<Utc>>)]
    pub kickoff_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Distinguishes "field absent" (None) from "field set to null" (Some(None)).
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Playlist {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub channel_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistItem {
    /// 1-based position; emitted as tvg-chno.
    pub position: i64,
    pub channel: Channel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistDetail {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub items: Vec<PlaylistItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistCreate {
    pub slug: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct PlaylistPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ItemAdd {
    pub channel_id: i64,
    /// 1-based insert position; appended when absent or beyond the end.
    #[serde(default)]
    pub position: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ItemsReplace {
    /// Full new order of channel ids.
    pub channel_ids: Vec<i64>,
}

/// App-wide settings. Service addresses live in the integrations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Settings {
    /// Hours between scheduled EPG fetches (the first runs 3 minutes after start); 0 turns the schedule off.
    pub epg_refresh_hours: u32,
    /// Increases with every change; of two copies of the settings, the higher revision is newer.
    /// Read-only.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_refresh_hours: Option<u32>,
}

/// What an integration needs, by type. Tagged by `kind`, which matches the integration's kind.
/// A blank required URL means the integration is not configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IntegrationConfig {
    /// The Acestream engine (required) and, optionally, acexy in front of it.
    Acestream {
        /// Engine base URL, e.g. `http://media-host:6878`; blank = not configured.
        engine_url: String,
        /// acexy base URL. When set, playlists play through acexy (several viewers share one engine
        /// session) and channel checks become available; when null, playlists use the engine directly.
        acexy_url: Option<String>,
        /// Hours between scheduled checks of all enabled acestream channels (the first runs 10
        /// minutes after start); 0 turns them off. Checks need acexy.
        probe_interval_hours: u32,
    },
    /// The VK proxy (a thin layer over streamlink).
    Vk {
        /// Proxy base URL; blank = not configured.
        url: String,
    },
    /// Direct URLs, emitted as they are. Nothing to configure.
    Direct {},
}

impl IntegrationConfig {
    pub fn kind(&self) -> SourceKind {
        match self {
            IntegrationConfig::Acestream { .. } => SourceKind::Acestream,
            IntegrationConfig::Vk { .. } => SourceKind::Vk,
            IntegrationConfig::Direct {} => SourceKind::Direct,
        }
    }

    /// The blank configuration a new database starts with.
    pub fn blank(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Acestream => IntegrationConfig::Acestream {
                engine_url: String::new(),
                acexy_url: None,
                probe_interval_hours: 0,
            },
            SourceKind::Vk => IntegrationConfig::Vk { url: String::new() },
            SourceKind::Direct => IntegrationConfig::Direct {},
        }
    }

    /// Whether channels of this integration can be played: its required URL is set. Says nothing
    /// about whether the service is reachable right now.
    pub fn configured(&self) -> bool {
        match self {
            IntegrationConfig::Acestream { engine_url, .. } => !engine_url.is_empty(),
            IntegrationConfig::Vk { url } => !url.is_empty(),
            IntegrationConfig::Direct {} => true,
        }
    }
}

/// Whether a feature is available, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Availability {
    pub available: bool,
    /// Why it is unavailable (null when available).
    pub reason: Option<String>,
}

impl Availability {
    pub fn yes() -> Self {
        Availability {
            available: true,
            reason: None,
        }
    }

    pub fn no(reason: impl Into<String>) -> Self {
        Availability {
            available: false,
            reason: Some(reason.into()),
        }
    }
}

/// A service lineup plays channels through (one per source kind).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Integration {
    pub kind: SourceKind,
    pub config: IntegrationConfig,
    /// Its required URL is set (a stable property; see the status for reachability).
    pub configured: bool,
    /// Health checks of its channels (Check now and the schedule).
    pub checks: Availability,
    /// Channels of this integration.
    pub channel_count: i64,
    /// Increases with every change; of two copies, the higher revision is newer. Read-only.
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

/// Partial update of an integration's config, tagged by `kind` (must match the integration).
/// Absent fields are left unchanged; an empty string turns a required URL off.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum IntegrationPatch {
    Acestream {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine_url: Option<String>,
        /// `null` or an empty string removes acexy.
        #[serde(default, with = "double_option", skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Option<String>)]
        acexy_url: Option<Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probe_interval_hours: Option<u32>,
    },
    Vk {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Direct {},
}

impl IntegrationPatch {
    pub fn kind(&self) -> SourceKind {
        match self {
            IntegrationPatch::Acestream { .. } => SourceKind::Acestream,
            IntegrationPatch::Vk { .. } => SourceKind::Vk,
            IntegrationPatch::Direct {} => SourceKind::Direct,
        }
    }
}

/// A part of an integration with its own address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Component {
    /// The Acestream engine.
    Engine,
    /// acexy, the Acestream proxy.
    Acexy,
    /// The VK proxy.
    Proxy,
}

impl Component {
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Engine => "engine",
            Component::Acexy => "acexy",
            Component::Proxy => "proxy",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConnectionTestRequest {
    pub component: Component,
    /// A base URL to test instead of the saved one (e.g. an unsaved edit).
    #[serde(default)]
    pub url: Option<String>,
}

/// Outcome of a connection test, run from the lineup server (it cannot show whether your players
/// can reach the same address).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConnectionTestResult {
    pub component: Component,
    pub ok: bool,
    /// The URL that was requested.
    pub url: String,
    /// What the answer shows, or why the test failed.
    pub detail: String,
    pub http_status: Option<u16>,
    /// Service version, when it reports one (the engine does; acexy does not).
    pub version: Option<String>,
    /// Streams acexy is serving now (acexy only).
    pub active_streams: Option<u64>,
    pub elapsed_ms: u64,
}

/// Live state of an integration's services (from the saved config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct IntegrationStatus {
    pub kind: SourceKind,
    pub configured: bool,
    /// One result per configured component; empty when nothing is configured (or for Direct).
    pub components: Vec<ConnectionTestResult>,
}

/// Whether acexy reports a stream as being watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LiveState {
    /// Someone is watching through acexy; the numbers are live.
    Active,
    /// Nobody is watching through acexy.
    Inactive,
    /// Cannot be told (no acexy, acexy unreachable, or no per-stream status).
    Unavailable,
}

/// Live numbers of an acestream channel, read from the session acexy already has open. Reading them
/// never starts or stops a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcestreamLive {
    pub state: LiveState,
    /// Why the state is unavailable, or why the numbers are missing.
    pub reason: Option<String>,
    /// Players connected to acexy for this stream.
    pub clients: Option<u64>,
    /// Engine session status (`prebuf`, `dl`, ...).
    pub status: Option<String>,
    pub peers: Option<u64>,
    /// Download speed, KB/s.
    pub speed_down: Option<u64>,
    /// Upload speed, KB/s.
    pub speed_up: Option<u64>,
    /// Bytes downloaded in this session.
    pub downloaded: Option<u64>,
    /// Bytes uploaded in this session.
    pub uploaded: Option<u64>,
}

/// Acestream section of a channel's details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcestreamInfo {
    /// The name the engine reports for this content id (only when asked for with `name=true`).
    pub engine_name: Option<String>,
    /// Why the name could not be read.
    pub engine_name_error: Option<String>,
    pub live: AcestreamLive,
}

/// What lineup can tell about a channel right now, beyond its stored fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChannelInfo {
    pub channel_id: i64,
    pub kind: SourceKind,
    /// Whether the channel's integration is configured.
    pub configured: bool,
    /// Whether "Check now" can run for this channel.
    pub checks: Availability,
    /// Latest check result and last conclusive result (null until checked).
    pub health: Option<ChannelHealth>,
    /// For acestream channels.
    pub acestream: Option<AcestreamInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveRequest {
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveResponse {
    pub source: Source,
    pub source_key: String,
    /// Null while the source's integration is not configured.
    pub stream_url: Option<String>,
    /// Whether the source's integration is configured.
    pub configured: bool,
    /// Existing channel with the same source, if any.
    pub duplicate_of: Option<i64>,
    /// Name the Acestream engine reports for this content id, as-is.
    #[serde(default)]
    pub engine_name: Option<String>,
    /// Suggested values for a new channel; shown as editable defaults.
    pub suggestion: Suggestion,
    /// Best guide channels for the suggested name, best first (up to 5).
    #[serde(default)]
    pub matches: Vec<EpgChannel>,
    /// Problems that did not stop the resolve, such as an unreachable engine.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Suggestion {
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    /// VK links are usually one-off live events.
    pub is_event: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportRequest {
    /// Raw m3u text (exactly one of `m3u_text` / `url`).
    #[serde(default)]
    pub m3u_text: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub playlist_slug: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportAction {
    /// A new channel is created and appended.
    Create,
    /// An existing channel with the same source is reused and appended.
    Reuse,
    /// The channel (new or existing) is already in the playlist.
    SkipInPlaylist,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportEntry {
    /// 1-based entry index in the file.
    pub index: usize,
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub source: Source,
    pub source_key: String,
    pub action: ImportAction,
    pub channel_id: Option<i64>,
    /// Fields where the file differs from the existing channel (existing metadata is never overwritten).
    pub diff: Vec<FieldDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FieldDiff {
    pub field: String,
    pub existing: Option<String>,
    pub incoming: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportReport {
    pub dry_run: bool,
    pub playlist_slug: String,
    pub playlist_created: bool,
    pub entries: Vec<ImportEntry>,
    /// Lines that could not be parsed into an entry.
    pub skipped_lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RefreshRequest {
    /// Playlists to refresh; all when absent or empty.
    #[serde(default)]
    pub slugs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RefreshResponse {
    pub slugs: Vec<String>,
}

/// An XMLTV guide source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EpgSource {
    pub id: i64,
    pub name: String,
    /// XMLTV URL, plain or gzipped.
    pub url: String,
    /// When several sources carry the same channel id, the highest priority wins.
    pub priority: i64,
    pub enabled: bool,
    /// Generation being served; null until the first successful fetch.
    pub current_gen: Option<i64>,
    pub last_fetch_at: Option<DateTime<Utc>>,
    /// `ok`, `not_modified` or `error`.
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    /// Channels in the served generation.
    pub channel_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EpgSourceCreate {
    pub url: String,
    /// Defaults to the URL's host.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EpgSourcePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EpgFetchRequest {
    /// Sources to fetch; all enabled sources when empty.
    #[serde(default)]
    pub source_ids: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpgFetchStatus {
    /// A new generation was downloaded and published.
    Updated,
    /// The server answered 304; the current generation stays.
    NotModified,
    /// The source is disabled and was not fetched.
    Disabled,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EpgFetchResult {
    pub source_id: i64,
    pub status: EpgFetchStatus,
    pub error: Option<String>,
    /// Channels in the published generation (when updated).
    pub channels: Option<i64>,
    /// Programmes kept in the published generation (when updated).
    pub programmes: Option<i64>,
}

/// A channel offered by a guide source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EpgChannel {
    pub source_id: i64,
    pub xmltv_id: String,
    pub display_names: Vec<String>,
    pub icon: Option<String>,
    /// Match score in [0, 1] when the list came from a search.
    pub score: Option<f64>,
}

/// Event types a hook can subscribe to.
pub const EVENT_TYPES: &[&str] = &[
    "playlist.changed",
    "epg.updated",
    "channel.unhealthy",
    "channel.recovered",
];

/// Shown instead of a stored secret. Sending it back unchanged keeps the secret.
pub const SECRET_MASK: &str = "********";

/// What a hook calls. Tagged by `kind` so other kinds can be added later; today only webhooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HookConfig {
    /// Sends the event envelope as JSON to a URL (e.g. a media server's refresh endpoint).
    Webhook {
        #[serde(default = "default_method")]
        method: String,
        url: String,
        /// Extra request headers. Values are write-only: responses show a mask.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
}

fn default_method() -> String {
    "POST".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Hook {
    pub id: i64,
    pub name: String,
    pub config: HookConfig,
    /// Event types that trigger it.
    pub events: Vec<String>,
    /// Events are collected until none has arrived for this long, then delivered together.
    pub debounce_secs: u32,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HookCreate {
    pub name: String,
    pub config: HookConfig,
    /// Defaults to every event type.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub debounce_secs: Option<u32>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct HookPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Replaces the whole config; masked secrets sent back unchanged keep their stored values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HookConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_secs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryTrigger {
    /// Debounced events.
    Events,
    /// The test action.
    Test,
}

/// One delivery attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HookDelivery {
    pub id: i64,
    pub hook_id: i64,
    pub events: Vec<String>,
    pub trigger: DeliveryTrigger,
    /// 1 for the first try; retries count up.
    pub attempt: u32,
    /// The receiver answered 2xx (accepted; not a guarantee that it finished).
    pub ok: bool,
    pub http_code: Option<u16>,
    pub error: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Status {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code: not_found, conflict, invalid, internal.
    pub code: String,
    pub message: String,
}

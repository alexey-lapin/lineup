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
    /// Channels to probe; all enabled acestream channels when empty (that run continues in the
    /// background).
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

/// A channel's stream source. The full URL is built from the per-kind base URL in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// Acestream content id (40 lowercase hex chars), played through acexy.
    Acestream { id: String },
    /// VK video, played through the VK proxy.
    Vk {
        owner_id: i64,
        video_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list: Option<String>,
    },
    /// Any other URL, emitted verbatim.
    Raw { url: String },
}

impl Source {
    pub fn kind(&self) -> SourceKind {
        match self {
            Source::Acestream { .. } => SourceKind::Acestream,
            Source::Vk { .. } => SourceKind::Vk,
            Source::Raw { .. } => SourceKind::Raw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Acestream,
    Vk,
    Raw,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Acestream => "acestream",
            SourceKind::Vk => "vk",
            SourceKind::Raw => "raw",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "acestream" => Some(SourceKind::Acestream),
            "vk" => Some(SourceKind::Vk),
            "raw" => Some(SourceKind::Raw),
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
    /// Stream URL as currently emitted in playlists.
    pub stream_url: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Settings {
    pub acexy_base: String,
    pub engine_base: String,
    pub vk_base: String,
    /// Hours between scheduled EPG fetches (the first runs 3 minutes after start); 0 turns the schedule off.
    pub epg_refresh_hours: u32,
    /// Hours between scheduled health probes of all acestream channels (the first runs 10 minutes
    /// after start); 0, the default, turns them off.
    pub probe_interval_hours: u32,
    /// Increases with every change; of two copies of the settings, the higher revision is newer.
    /// Read-only.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acexy_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vk_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_refresh_hours: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_interval_hours: Option<u32>,
}

/// A service whose base URL can be tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SettingsTestTarget {
    Acexy,
    Engine,
    Vk,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettingsTestRequest {
    pub target: SettingsTestTarget,
    /// A base URL to test instead of the saved one (e.g. an unsaved edit).
    #[serde(default)]
    pub base: Option<String>,
}

/// Outcome of a connection test, run from the lineup server (it cannot show whether your players
/// can reach the same address).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SettingsTestResult {
    pub ok: bool,
    /// The URL that was requested.
    pub url: String,
    /// What the answer shows, or why the test failed.
    pub detail: String,
    pub http_status: Option<u16>,
    /// Engine version, when testing the engine.
    pub version: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveRequest {
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveResponse {
    pub source: Source,
    pub source_key: String,
    pub stream_url: String,
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

/// What a hook calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HookConfig {
    /// Runs Jellyfin's "Refresh Guide" scheduled task.
    Jellyfin {
        /// e.g. `http://jellyfin:8096`
        base_url: String,
        /// API key (Dashboard → API Keys). Write-only: responses show a mask.
        api_key: String,
        /// Scheduled task id; found by its key `RefreshGuide` when empty.
        #[serde(default)]
        task_id: Option<String>,
        /// A second refresh this many minutes after the last delivery, once Jellyfin's cached
        /// guide download has expired; 0 turns it off.
        #[serde(default = "default_followup_minutes")]
        followup_minutes: u32,
    },
    /// Sends the event envelope as JSON.
    Webhook {
        #[serde(default = "default_method")]
        method: String,
        url: String,
        /// Extra request headers. Values are write-only: responses show a mask.
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
}

fn default_followup_minutes() -> u32 {
    65
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
    /// Defaults: `playlist.changed` and `epg.updated` for Jellyfin, every event for webhooks.
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
    /// The Jellyfin follow-up refresh.
    Followup,
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

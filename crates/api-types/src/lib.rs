//! Wire types shared by the lineup server, CLI and (via OpenAPI) the web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// -------------------------------------------------------------------- health

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CheckOutcome {
    /// Playable.
    Ok,
    /// Not playable.
    Dead,
    /// Could not tell (the gateway, its services or the network failed); says nothing about the
    /// channel.
    Unknown,
    /// Not checked, for example because the channel was being watched.
    Skipped,
}

impl CheckOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckOutcome::Ok => "ok",
            CheckOutcome::Dead => "dead",
            CheckOutcome::Unknown => "unknown",
            CheckOutcome::Skipped => "skipped",
        }
    }
}

/// What a page source currently is, when the gateway can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentState {
    Live,
    /// The live broadcast ended and the recording is not playable yet.
    EndedProcessing,
    Recording,
}

/// Check results of a channel. Only `ok` and `dead` are conclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChannelHealth {
    /// Last conclusive result (`ok` or `dead`); drives the badge and events.
    pub conclusive: Option<CheckOutcome>,
    pub conclusive_at: Option<DateTime<Utc>>,
    pub last_attempt: CheckOutcome,
    pub last_attempt_at: DateTime<Utc>,
    /// Content state reported by the last attempt.
    pub content_state: Option<ContentState>,
    /// Why the last attempt was `unknown` or `skipped` (e.g. `busy`).
    pub reason: Option<String>,
    /// Gateway details of the last attempt.
    #[schema(value_type = Object)]
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CheckRequest {
    /// Channels to check; all enabled channels of routes that support checks when empty (that run
    /// continues in the background).
    #[serde(default)]
    pub ids: Vec<i64>,
    /// Check even if the channel seems to be in use.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckResult {
    pub channel_id: i64,
    pub outcome: CheckOutcome,
    pub reason: Option<String>,
    pub content_state: Option<ContentState>,
    #[schema(value_type = Object)]
    pub detail: Option<serde_json::Value>,
    /// False when the result was not stored (the channel or its route changed meanwhile, or the
    /// check did not run).
    pub recorded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckResponse {
    /// True when a run over all channels was started in the background; results then arrive as
    /// channel updates and events.
    pub background: bool,
    /// Results, when specific channels were checked.
    pub results: Vec<CheckResult>,
}

// -------------------------------------------------------------------- routes

/// How a route plays its channels. Tagged by `kind`; the kind and a gateway's rule can't change
/// after the route is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RouteConfig {
    /// Channels are stream URLs, emitted as they are.
    Direct {},
    /// A gateway service turns channel sources into play URLs.
    Gateway {
        /// Gateway base URL, as players reach it (e.g. `http://media-host:8099`).
        base_url: String,
        /// One of the gateway's rules (e.g. `acestream`, `vk`).
        rule: String,
    },
}

impl RouteConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            RouteConfig::Direct {} => "direct",
            RouteConfig::Gateway { .. } => "gateway",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Route {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub config: RouteConfig,
    pub channel_count: i64,
    /// Increases with every change; of two copies, the higher revision is newer. Read-only.
    pub revision: u64,
    /// The rule's match revision at the last resync that updated every channel (gateway only).
    pub synced_match_revision: Option<String>,
    /// The rule's play revision at the last resync that updated every channel (gateway only).
    pub synced_play_revision: Option<String>,
    pub last_resync: Option<ResyncReport>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteCreate {
    pub name: String,
    pub config: RouteConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Partial update. The kind and a gateway's rule can't change; create another route instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RoutePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New gateway base URL. Every channel of the route is revalidated against it first; if any
    /// fails, nothing changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// A gateway rule, as the gateway describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSummary {
    pub name: String,
    pub title: String,
    pub input_hint: Option<String>,
    /// `observe`, `check`.
    pub capabilities: Vec<String>,
    pub match_revision: String,
    pub play_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GatewaySummary {
    pub name: String,
    pub version: String,
    pub contract: u32,
    pub rules: Vec<RuleSummary>,
}

/// Test a route config before (or without) saving it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RouteTestRequest {
    pub config: RouteConfig,
    /// Also validate this input with the config.
    #[serde(default)]
    pub input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RouteTestResult {
    pub ok: bool,
    /// What the test shows, or why it failed.
    pub detail: String,
    /// The gateway's self-description (gateway configs that answered).
    pub gateway: Option<GatewaySummary>,
    /// How the input validated, when one was given and the route works.
    pub sample: Option<ValidatedSource>,
    pub elapsed_ms: u64,
}

/// A source as a route validated it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ValidatedSource {
    /// Canonical source, stored with the channel.
    pub source: String,
    /// De-duplication key within the route.
    pub identity: String,
    /// The URL players get.
    pub play_url: String,
    pub hints: Hints,
    pub warnings: Vec<String>,
}

/// Live state of a route's gateway (from the saved config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RouteStatus {
    pub route_id: i64,
    /// The gateway answered and has the route's rule.
    pub ok: bool,
    pub detail: String,
    pub gateway: Option<GatewaySummary>,
    /// The route's rule as the gateway describes it now.
    pub rule: Option<RuleSummary>,
    /// Channels whose play reference is from another play revision than the gateway's current one
    /// (they need a resync). Null when the gateway did not answer.
    pub pending_channels: Option<i64>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResyncTrigger {
    Manual,
    BaseUrlChange,
    RevisionChange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResyncItem {
    pub channel_id: i64,
    pub name: String,
    pub reason: String,
}

/// Outcome of revalidating every channel of a route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResyncReport {
    pub at: DateTime<Utc>,
    pub trigger: ResyncTrigger,
    pub match_revision: Option<String>,
    pub play_revision: Option<String>,
    pub total: usize,
    /// Channels whose source, identity or play reference changed.
    pub updated: usize,
    pub unchanged: usize,
    /// Channels the gateway did not accept, or that would collide with another channel; they keep
    /// their previous values.
    pub failed: Vec<ResyncItem>,
    /// Channels changed by someone else during the resync; left as they are.
    pub skipped: Vec<ResyncItem>,
    /// Every channel is now current (the route's synced revisions advanced).
    pub complete: bool,
}

/// Suggestions a gateway gives for a source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Hints {
    pub title: Option<String>,
    /// Two-letter lowercase region, e.g. `de`.
    pub region: Option<String>,
    pub group: Option<String>,
    /// Usually a one-off event.
    pub event: Option<bool>,
}

// -------------------------------------------------------------------- channels

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
    pub route_id: i64,
    pub route_name: String,
    /// Whether the channel's route is enabled. Channels of a disabled route stay listed but are
    /// left out of playlists and guides.
    pub route_enabled: bool,
    /// Canonical source, as the route validated it.
    pub source: String,
    /// De-duplication key (unique per route).
    pub identity: String,
    /// The URL emitted in playlists.
    pub play_url: String,
    pub is_event: bool,
    pub kickoff_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    /// Slugs of playlists containing this channel.
    pub playlists: Vec<String>,
    /// Check results; null until checked.
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
    /// The route that plays the channel.
    pub route_id: i64,
    /// What to play, in a form the route accepts (a URL for Direct; the gateway rule's input).
    pub input: String,
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

fn default_true() -> bool {
    true
}

/// Partial update. Absent fields are left unchanged; for nullable fields, `null` clears.
/// `route_id` and/or `input` revalidate the source (with the current source when only the route
/// changes).
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
    pub route_id: Option<i64>,
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

/// App-wide settings. Service addresses live in the routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Settings {
    /// Hours between scheduled EPG fetches (the first runs 3 minutes after start); 0 turns the schedule off.
    pub epg_refresh_hours: u32,
    /// Hours between scheduled checks of all enabled channels on routes that support checks (the
    /// first runs 10 minutes after start); 0 turns them off.
    pub check_interval_hours: u32,
    /// Increases with every change; of two copies of the settings, the higher revision is newer.
    /// Read-only.
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_refresh_hours: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_interval_hours: Option<u32>,
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

/// A labelled value reported by a gateway, for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Fact {
    pub label: String,
    pub value: String,
}

/// What the route's gateway reports about a channel right now. Reading it never starts a stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Observation {
    /// Whether anything could be observed.
    pub available: bool,
    /// Why not, or why some values are missing.
    pub reason: Option<String>,
    pub content_state: Option<ContentState>,
    /// Whether someone is watching through the gateway's services, when known.
    pub watching: Option<bool>,
    pub facts: Vec<Fact>,
    /// The source's current title, when it reports one.
    pub title: Option<String>,
}

/// What lineup can tell about a channel right now, beyond its stored fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChannelInfo {
    pub channel_id: i64,
    pub route_id: i64,
    /// Whether "Check now" can run for this channel.
    pub checks: Availability,
    /// Latest check result and last conclusive result (null until checked).
    pub health: Option<ChannelHealth>,
    /// Null when the route can't observe (Direct, or a rule without `observe`).
    pub observation: Option<Observation>,
    /// Why there is no observation although the route supports it (e.g. the gateway is unreachable).
    pub observation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveRequest {
    pub route_id: i64,
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResolveResponse {
    pub route_id: i64,
    #[serde(flatten)]
    pub validated: ValidatedSource,
    /// Existing channel on this route with the same identity, if any.
    pub duplicate_of: Option<i64>,
    /// The duplicate's stored source, when it differs from this one (e.g. a newer access token);
    /// editing that channel with this input updates it.
    pub duplicate_source: Option<String>,
    /// Suggested values for a new channel; shown as editable defaults.
    pub suggestion: Suggestion,
    /// Best guide channels for the suggested name, best first (up to 5).
    #[serde(default)]
    pub matches: Vec<EpgChannel>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Suggestion {
    pub name: String,
    pub group_title: Option<String>,
    pub tvg_id: Option<String>,
    pub tvg_logo: Option<String>,
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
    /// The route every entry is validated with. Entries it does not accept are skipped and listed.
    pub route_id: i64,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportAction {
    /// A new channel is created and appended.
    Create,
    /// An existing channel with the same identity on the route is reused and appended.
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
    /// Canonical source from the route.
    pub source: String,
    pub identity: String,
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
    /// Stable machine-readable code: not_found, conflict, invalid, internal, gateway_unreachable,
    /// gateway_error, contract_mismatch.
    pub code: String,
    pub message: String,
}

//! Wire types shared by the lineup server, CLI and (via OpenAPI) the web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acexy_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vk_base: Option<String>,
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

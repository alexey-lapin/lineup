//! `lineup` command line: argument definitions and the HTTP client commands.
//!
//! `serve` is defined here but executed by the `lineup` binary (which links the server).

mod client;
mod commands;
mod output;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};

pub use client::{CliError, exit_code};

#[derive(Debug, Parser)]
#[command(name = "lineup", version, about = "Manage Jellyfin Live TV channels and playlists")]
pub struct Cli {
    /// Server URL [default: from ~/.config/lineup/config.toml, else http://127.0.0.1:9300]
    #[arg(long, global = true, env = "LINEUP_SERVER")]
    pub server: Option<String>,
    /// Print machine-readable JSON (the API response) instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Only print essentials (ids, URLs); no tables.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the lineup server.
    Serve(ServeArgs),
    /// Add channels from inputs (acestream://id, 40-hex id, vk.com URL, VK proxy URL, any URL).
    Add(AddArgs),
    /// List channels, optionally filtered by a search term.
    Ls(LsArgs),
    /// Show one channel.
    Show { channel: String },
    /// Edit a channel.
    Edit(EditArgs),
    /// Delete channels.
    Rm {
        #[arg(required = true)]
        channels: Vec<String>,
    },
    /// Enable channels (they appear in playlists again).
    Enable {
        #[arg(required = true)]
        channels: Vec<String>,
    },
    /// Disable channels (kept, but left out of playlist output; numbering is unchanged).
    Disable {
        #[arg(required = true)]
        channels: Vec<String>,
    },
    /// Parse an input and show the source, stream URL, any duplicate and suggestions, without saving.
    Resolve { input: String },
    /// Manage playlists.
    #[command(subcommand)]
    Playlist(PlaylistCmd),
    /// Import an m3u file (path, URL, or - for stdin) into a playlist.
    Import(ImportArgs),
    /// Manage guide (XMLTV) sources and search their channels.
    #[command(subcommand)]
    Epg(EpgCmd),
    /// Manage hooks (Jellyfin guide refresh, webhooks).
    #[command(subcommand)]
    Hook(HookCmd),
    /// Re-emit change notifications for playlists (all when none are given).
    Refresh { slugs: Vec<String> },
    /// Show or change settings.
    #[command(subcommand)]
    Settings(SettingsCmd),
    /// Download a consistent copy of the server's database (restore: stop lineup, replace /data/lineup.db).
    Backup {
        /// Output file (must not exist).
        path: PathBuf,
    },
    /// Server status.
    Status,
    /// Print the CLI's usage spec (https://usage.jdx.dev) for completions and docs.
    Usage,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address to listen on.
    #[arg(long, env = "LINEUP_BIND", default_value = "0.0.0.0:9300")]
    pub bind: SocketAddr,
    /// Directory for the database and caches.
    #[arg(long, env = "LINEUP_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(required = true)]
    pub inputs: Vec<String>,
    /// Display name (only with a single input). Defaults to a name derived from the source.
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long)]
    pub tvg_id: Option<String>,
    #[arg(long)]
    pub logo: Option<String>,
    /// Append to this playlist (repeatable).
    #[arg(long = "playlist", short = 'p')]
    pub playlists: Vec<String>,
    /// Mark as a live event (VK match etc.).
    #[arg(long)]
    pub event: bool,
    /// Event kickoff time (RFC 3339, e.g. 2026-11-05T20:00:00Z).
    #[arg(long)]
    pub kickoff: Option<chrono::DateTime<chrono::Utc>>,
    /// Create disabled.
    #[arg(long)]
    pub disabled: bool,
    /// Show what would be added without saving.
    #[arg(long)]
    pub dry_run: bool,
    /// Accept the suggested group, tvg-id, logo and event flag without asking. Explicit flags win.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Search name, tvg-id or source key.
    pub query: Option<String>,
    #[arg(long)]
    pub group: Option<String>,
    #[arg(long, short = 'p')]
    pub playlist: Option<String>,
    #[arg(long, value_enum)]
    pub kind: Option<KindArg>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum KindArg {
    Acestream,
    Vk,
    Raw,
}

impl KindArg {
    pub fn as_str(self) -> &'static str {
        match self {
            KindArg::Acestream => "acestream",
            KindArg::Vk => "vk",
            KindArg::Raw => "raw",
        }
    }
}

#[derive(Debug, Args)]
pub struct EditArgs {
    pub channel: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, conflicts_with = "no_group")]
    pub group: Option<String>,
    #[arg(long)]
    pub no_group: bool,
    #[arg(long, conflicts_with = "no_tvg_id")]
    pub tvg_id: Option<String>,
    #[arg(long)]
    pub no_tvg_id: bool,
    #[arg(long, conflicts_with = "no_logo")]
    pub logo: Option<String>,
    #[arg(long)]
    pub no_logo: bool,
    /// New source (any input form accepted by `add`).
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long, conflicts_with = "no_event")]
    pub event: bool,
    #[arg(long)]
    pub no_event: bool,
    #[arg(long, conflicts_with = "no_kickoff")]
    pub kickoff: Option<chrono::DateTime<chrono::Utc>>,
    #[arg(long)]
    pub no_kickoff: bool,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistCmd {
    /// List playlists.
    Ls,
    /// Show a playlist's channels in order.
    Show { slug: String },
    /// Create a playlist.
    Create {
        slug: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Rename a playlist (display name and/or slug).
    Rename {
        slug: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "slug")]
        new_slug: Option<String>,
    },
    /// Delete a playlist (channels are kept).
    Rm { slug: String },
    /// Add channels to a playlist.
    Add {
        slug: String,
        #[arg(required = true)]
        channels: Vec<String>,
        /// 1-based position for the first channel (others follow it).
        #[arg(long)]
        at: Option<i64>,
    },
    /// Remove channels from a playlist.
    Remove {
        slug: String,
        #[arg(required = true)]
        channels: Vec<String>,
    },
    /// Move a channel to a 1-based position.
    Move {
        slug: String,
        channel: String,
        position: i64,
    },
    /// Replace the playlist's channels with exactly these, in this order.
    Set { slug: String, channels: Vec<String> },
    /// Print the playlist URL(s).
    Url { slug: String },
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// File path, http(s) URL (fetched by the server), or - for stdin.
    pub source: String,
    #[arg(long, short = 'p')]
    pub playlist: String,
    /// Preview only; nothing is written.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum EpgCmd {
    /// Fetch sources now (all enabled ones when no ids are given) and wait for the result.
    Fetch { ids: Vec<i64> },
    /// Manage sources.
    #[command(subcommand)]
    Sources(EpgSourcesCmd),
    /// Search the sources' channels by name or id (for picking a tvg-id).
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
pub enum EpgSourcesCmd {
    /// List sources with their last fetch status.
    Ls,
    /// Add an XMLTV URL (plain or gzipped). The first fetch starts right away.
    Add {
        url: String,
        #[arg(long)]
        name: Option<String>,
        /// Higher wins when sources carry the same channel id.
        #[arg(long, default_value_t = 0)]
        priority: i64,
    },
    /// Change a source.
    Edit {
        id: i64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        priority: Option<i64>,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
    },
    /// Delete a source and its downloaded data.
    Rm { id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum HookCmd {
    /// List hooks (secrets are masked).
    Ls,
    /// Add a hook.
    #[command(subcommand)]
    Add(HookAddCmd),
    /// Enable a hook.
    Enable { id: i64 },
    /// Disable a hook.
    Disable { id: i64 },
    /// Delete a hook and its delivery log.
    Rm { id: i64 },
    /// Send a test event now (for Jellyfin this really starts a guide refresh).
    Test { id: i64 },
    /// Show recent delivery attempts, newest first.
    Deliveries {
        id: i64,
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
}

#[derive(Debug, Args)]
pub struct HookCommon {
    /// Event types (comma-separated): playlist.changed, epg.updated, channel.unhealthy, channel.recovered.
    #[arg(long, value_delimiter = ',')]
    pub events: Option<Vec<String>>,
    /// Seconds without new events before delivering (default 30).
    #[arg(long)]
    pub debounce: Option<u32>,
}

#[derive(Debug, Subcommand)]
pub enum HookAddCmd {
    /// Run Jellyfin's Refresh Guide task when playlists or guide data change.
    Jellyfin {
        name: String,
        /// Jellyfin base URL, e.g. http://jellyfin:8096
        #[arg(long)]
        url: String,
        /// Jellyfin API key (Dashboard → API Keys).
        #[arg(long, env = "LINEUP_JELLYFIN_API_KEY")]
        api_key: String,
        /// Scheduled task id; looked up by key RefreshGuide when omitted.
        #[arg(long)]
        task_id: Option<String>,
        /// Second refresh this many minutes after the last delivery (0 = off).
        #[arg(long, default_value_t = 65)]
        followup_minutes: u32,
        #[command(flatten)]
        common: HookCommon,
    },
    /// POST (or PUT/PATCH) the event envelope as JSON to a URL.
    Webhook {
        name: String,
        url: String,
        #[arg(long, default_value = "POST")]
        method: String,
        /// Extra header, as Name=value (repeatable).
        #[arg(long = "header", value_parser = parse_header)]
        headers: Vec<(String, String)>,
        #[command(flatten)]
        common: HookCommon,
    },
}

fn parse_header(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| format!("expected Name=value, got {s:?}"))
}

#[derive(Debug, Subcommand)]
pub enum SettingsCmd {
    /// Show all settings.
    Get,
    /// Set one setting: acexy_base, engine_base, vk_base or epg_refresh_hours.
    Set { key: String, value: String },
}

/// What the binary should do after parsing.
pub enum Dispatch {
    Serve(ServeArgs),
    Client(Cli),
}

pub fn parse() -> Dispatch {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => Dispatch::Serve(args),
        _ => Dispatch::Client(cli),
    }
}

/// Run a client command; returns the process exit code.
pub async fn run(cli: Cli) -> i32 {
    if matches!(cli.command, Command::Usage) {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_usage::generate(&mut cmd, "lineup", &mut buf);
        // Ignore broken pipes (e.g. `lineup usage | head`).
        let _ = std::io::Write::write_all(&mut std::io::stdout(), &buf);
        return 0;
    }
    let json = cli.json;
    match commands::run(cli).await {
        Ok(()) => 0,
        Err(CliError::Reported(code)) => code,
        Err(e) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({"error": {"code": e.code(), "message": e.to_string()}})
                );
            } else {
                eprintln!("error: {e}");
            }
            exit_code(&e)
        }
    }
}

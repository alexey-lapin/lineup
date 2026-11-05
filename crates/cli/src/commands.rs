use api_types::{
    Channel, ChannelCreate, ChannelPatch, ImportReport, ImportRequest, ItemAdd, ItemsReplace, Playlist, PlaylistCreate,
    PlaylistDetail, PlaylistPatch, RefreshRequest, RefreshResponse, ResolveRequest, ResolveResponse, Settings,
    SettingsPatch, Source, Status,
};
use serde::Serialize;

use crate::client::{CliError, Client};
use crate::{Cli, Command, PlaylistCmd, SettingsCmd, output};

type R<T = ()> = Result<T, CliError>;

const DEFAULT_SERVER: &str = "http://127.0.0.1:9300";

struct Ctx {
    c: Client,
    json: bool,
    quiet: bool,
    /// Lazily loaded channel list for resolving name references.
    channels: Option<Vec<Channel>>,
}

impl Ctx {
    fn emit<T: Serialize>(&self, v: &T, human: impl FnOnce(&T)) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        } else {
            human(v);
        }
    }

    /// Resolve a channel reference: numeric id, exact name (case-insensitive), or a source input.
    async fn channel_id(&mut self, r: &str) -> R<i64> {
        if let Ok(id) = r.parse::<i64>() {
            return Ok(id);
        }
        if self.channels.is_none() {
            self.channels = Some(self.c.get("/channels").await?);
        }
        let matches: Vec<&Channel> = self
            .channels
            .as_ref()
            .unwrap()
            .iter()
            .filter(|c| c.name.eq_ignore_ascii_case(r))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].id),
            n if n > 1 => {
                let ids: Vec<String> = matches.iter().map(|c| c.id.to_string()).collect();
                return Err(CliError::Usage(format!(
                    "{r:?} matches {n} channels (ids {}); use an id",
                    ids.join(", ")
                )));
            }
            _ => {}
        }
        // Not a name: try it as a source input. An unparseable input simply means "no match";
        // any other failure (server error, unreachable) is reported as such.
        match self
            .c
            .post::<ResolveResponse, _>("/resolve", &ResolveRequest { input: r.into() })
            .await
        {
            Ok(res) => {
                if let Some(id) = res.duplicate_of {
                    return Ok(id);
                }
            }
            Err(CliError::Api { code, .. }) if code == "invalid" => {}
            Err(e) => return Err(e),
        }
        Err(CliError::Api {
            code: "not_found".into(),
            message: format!("no channel matches {r:?}"),
        })
    }
}

fn server_url(flag: Option<String>) -> R<String> {
    if let Some(s) = flag {
        return Ok(s);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::Path::new(&home).join(".config/lineup/config.toml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            #[derive(serde::Deserialize)]
            struct Config {
                server: Option<String>,
            }
            let cfg: Config = toml::from_str(&text).map_err(|e| CliError::Usage(format!("{}: {e}", path.display())))?;
            if let Some(s) = cfg.server {
                return Ok(s);
            }
        }
    }
    Ok(DEFAULT_SERVER.into())
}

fn enc(s: &str) -> String {
    // Slugs are [a-z0-9_-]; encode defensively anyway.
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn default_name(s: &Source) -> String {
    match s {
        Source::Acestream { id } => format!("Acestream {}", &id[..8]),
        Source::Vk { owner_id, video_id, .. } => format!("VK {owner_id}_{video_id}"),
        Source::Raw { url } => url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| url.clone()),
    }
}

pub async fn run(cli: Cli) -> R {
    let mut x = Ctx {
        c: Client::new(&server_url(cli.server)?)?,
        json: cli.json,
        quiet: cli.quiet,
        channels: None,
    };
    match cli.command {
        Command::Serve(_) | Command::Usage => unreachable!("handled by caller"),

        Command::Backup { path } => {
            if path.exists() {
                return Err(CliError::Usage(format!("{} already exists", path.display())));
            }
            let bytes = x.c.get_bytes("/backup").await?;
            if !bytes.starts_with(b"SQLite format 3\0") {
                return Err(CliError::Other("server did not return a SQLite database".into()));
            }
            // create_new: never overwrite, even a file that appeared during the download.
            let write = || -> std::io::Result<()> {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
                if let Err(e) = f.write_all(&bytes).and_then(|_| f.sync_all()) {
                    drop(f);
                    let _ = std::fs::remove_file(&path); // no partial backups
                    return Err(e);
                }
                Ok(())
            };
            write().map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => CliError::Usage(format!("{} already exists", path.display())),
                _ => CliError::Other(format!("writing {}: {e}", path.display())),
            })?;
            let info = serde_json::json!({ "path": path.display().to_string(), "bytes": bytes.len() });
            x.emit(&info, |_| println!("wrote {} ({} bytes)", path.display(), bytes.len()));
        }

        Command::Status => {
            let s: Status = x.c.get("/status").await?;
            let base = x.c.base().to_string();
            x.emit(&s, |s| output::status(s, &base));
        }

        Command::Resolve { input } => {
            let r: ResolveResponse = x.c.post("/resolve", &ResolveRequest { input }).await?;
            x.emit(&r, |r| {
                println!("source:     {}", serde_json::to_string(&r.source).unwrap_or_default());
                println!("key:        {}", r.source_key);
                println!("stream url: {}", r.stream_url);
                match r.duplicate_of {
                    Some(id) => println!("duplicate:  yes, channel {id}"),
                    None => println!("duplicate:  no"),
                }
            });
        }

        Command::Add(a) => {
            if a.name.is_some() && a.inputs.len() > 1 {
                return Err(CliError::Usage("--name can only be used with a single input".into()));
            }
            // One result per input. JSON output is always an array of
            // {input, ok, channel | would_create, error: {code, message}}.
            let mut results = Vec::new();
            let mut errors: Vec<CliError> = Vec::new();
            for input in &a.inputs {
                match add_one(&x, &a, input).await {
                    Ok(AddOutcome::Created(ch)) => {
                        if !x.json {
                            if x.quiet {
                                println!("{}", ch.id);
                            } else {
                                println!("added {} {:?} -> {}", ch.id, ch.name, ch.stream_url);
                            }
                        }
                        results.push(serde_json::json!({ "input": input, "ok": true, "channel": ch }));
                    }
                    Ok(AddOutcome::WouldCreate(req, url)) => {
                        if !x.json {
                            println!(
                                "would add {:?} ({}) -> {url}",
                                req.name,
                                output::source_label(req.source.as_ref().unwrap())
                            );
                        }
                        results.push(serde_json::json!({ "input": input, "ok": true, "would_create": req }));
                    }
                    Err(e) => {
                        if !x.json {
                            eprintln!("error: {input}: {e}");
                        }
                        results.push(serde_json::json!({
                            "input": input, "ok": false,
                            "error": { "code": e.code(), "message": e.to_string() }
                        }));
                        errors.push(e);
                    }
                }
            }
            if x.json {
                println!("{}", serde_json::to_string_pretty(&results).unwrap_or_default());
            }
            // Exit: 0 if all succeeded; if every input failed with the same kind of error, that
            // error's code; otherwise (partial success or mixed errors) 1.
            if !errors.is_empty() {
                let codes: Vec<i32> = errors.iter().map(crate::client::exit_code).collect();
                let code = if errors.len() == a.inputs.len() && codes.iter().all(|c| *c == codes[0]) {
                    codes[0]
                } else {
                    1
                };
                return Err(CliError::Reported(code));
            }
        }

        Command::Ls(l) => {
            #[derive(Serialize)]
            struct Q {
                #[serde(skip_serializing_if = "Option::is_none")]
                q: Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                group: Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                playlist: Option<String>,
                #[serde(skip_serializing_if = "Option::is_none")]
                kind: Option<String>,
            }
            let list: Vec<Channel> =
                x.c.get_query(
                    "/channels",
                    &Q {
                        q: l.query,
                        group: l.group,
                        playlist: l.playlist,
                        kind: l.kind.map(|k| k.as_str().to_string()),
                    },
                )
                .await?;
            if x.quiet && !x.json {
                for c in &list {
                    println!("{}", c.id);
                }
            } else {
                x.emit(&list, |l| output::channels(l));
            }
        }

        Command::Show { channel } => {
            let id = x.channel_id(&channel).await?;
            let ch: Channel = x.c.get(&format!("/channels/{id}")).await?;
            x.emit(&ch, output::channel);
        }

        Command::Edit(e) => {
            let id = x.channel_id(&e.channel).await?;
            let tri = |set: Option<String>, clear: bool| if clear { Some(None) } else { set.map(Some) };
            let patch = ChannelPatch {
                name: e.name,
                group_title: tri(e.group, e.no_group),
                tvg_id: tri(e.tvg_id, e.no_tvg_id),
                tvg_logo: tri(e.logo, e.no_logo),
                source: None,
                input: e.source,
                is_event: if e.event {
                    Some(true)
                } else if e.no_event {
                    Some(false)
                } else {
                    None
                },
                kickoff_at: if e.no_kickoff { Some(None) } else { e.kickoff.map(Some) },
                enabled: None,
            };
            let ch: Channel = x.c.patch(&format!("/channels/{id}"), &patch).await?;
            x.emit(&ch, output::channel);
        }

        Command::Rm { channels } => {
            let mut done = Vec::new();
            for r in &channels {
                let id = x.channel_id(r).await?;
                let _: () = x.c.delete(&format!("/channels/{id}")).await?;
                if !x.json && !x.quiet {
                    println!("deleted {id}");
                }
                done.push(id);
            }
            if x.json {
                println!("{}", serde_json::json!({ "deleted": done }));
            }
        }

        Command::Enable { channels } => set_enabled(&mut x, &channels, true).await?,
        Command::Disable { channels } => set_enabled(&mut x, &channels, false).await?,

        Command::Playlist(p) => playlist(&mut x, p).await?,

        Command::Import(i) => {
            let (m3u_text, url) = if i.source == "-" {
                let mut s = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)
                    .map_err(|e| CliError::Other(format!("reading stdin: {e}")))?;
                (Some(s), None)
            } else if i.source.starts_with("http://") || i.source.starts_with("https://") {
                (None, Some(i.source.clone()))
            } else {
                let s = std::fs::read_to_string(&i.source)
                    .map_err(|e| CliError::Usage(format!("reading {}: {e}", i.source)))?;
                (Some(s), None)
            };
            let req = ImportRequest {
                m3u_text,
                url,
                playlist_slug: i.playlist,
                dry_run: i.dry_run,
            };
            let r: ImportReport = x.c.post("/import", &req).await?;
            x.emit(&r, output::import);
        }

        Command::Refresh { slugs } => {
            let r: RefreshResponse = x.c.post("/refresh", &RefreshRequest { slugs }).await?;
            x.emit(&r, |r| println!("refreshed: {}", r.slugs.join(", ")));
        }

        Command::Settings(SettingsCmd::Get) => {
            let s: Settings = x.c.get("/settings").await?;
            x.emit(&s, output::settings);
        }
        Command::Settings(SettingsCmd::Set { key, value }) => {
            let mut patch = SettingsPatch::default();
            match key.as_str() {
                "acexy_base" => patch.acexy_base = Some(value),
                "engine_base" => patch.engine_base = Some(value),
                "vk_base" => patch.vk_base = Some(value),
                other => return Err(CliError::Usage(format!("unknown setting {other:?}"))),
            }
            let s: Settings = x.c.patch("/settings", &patch).await?;
            x.emit(&s, output::settings);
        }
    }
    Ok(())
}

async fn set_enabled(x: &mut Ctx, channels: &[String], enabled: bool) -> R {
    let mut out = Vec::new();
    for r in channels {
        let id = x.channel_id(r).await?;
        let ch: Channel =
            x.c.patch(
                &format!("/channels/{id}"),
                &ChannelPatch {
                    enabled: Some(enabled),
                    ..Default::default()
                },
            )
            .await?;
        if !x.json && !x.quiet {
            println!(
                "{} {} {:?}",
                if enabled { "enabled" } else { "disabled" },
                ch.id,
                ch.name
            );
        }
        out.push(ch);
    }
    if x.json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    Ok(())
}

async fn playlist(x: &mut Ctx, cmd: PlaylistCmd) -> R {
    match cmd {
        PlaylistCmd::Ls => {
            let l: Vec<Playlist> = x.c.get("/playlists").await?;
            x.emit(&l, |l| output::playlists(l));
        }
        PlaylistCmd::Show { slug } => {
            let p: PlaylistDetail = x.c.get(&format!("/playlists/{}", enc(&slug))).await?;
            x.emit(&p, output::playlist);
        }
        PlaylistCmd::Create { slug, name } => {
            let p: PlaylistDetail = x.c.post("/playlists", &PlaylistCreate { slug, name }).await?;
            x.emit(&p, |p| println!("created {}", p.playlist.slug));
        }
        PlaylistCmd::Rename { slug, name, new_slug } => {
            let p: PlaylistDetail =
                x.c.patch(
                    &format!("/playlists/{}", enc(&slug)),
                    &PlaylistPatch { name, slug: new_slug },
                )
                .await?;
            x.emit(&p, |p| {
                println!("playlist is now {} ({})", p.playlist.slug, p.playlist.name)
            });
        }
        PlaylistCmd::Rm { slug } => {
            let _: () = x.c.delete(&format!("/playlists/{}", enc(&slug))).await?;
            x.emit(&serde_json::json!({ "deleted": slug }), |_| println!("deleted {slug}"));
        }
        PlaylistCmd::Add { slug, channels, at } => {
            let mut last = None;
            for (i, r) in channels.iter().enumerate() {
                let id = x.channel_id(r).await?;
                let p: PlaylistDetail =
                    x.c.post(
                        &format!("/playlists/{}/items", enc(&slug)),
                        &ItemAdd {
                            channel_id: id,
                            position: at.map(|a| a + i as i64),
                        },
                    )
                    .await?;
                last = Some(p);
            }
            if let Some(p) = last {
                x.emit(&p, output::playlist);
            }
        }
        PlaylistCmd::Remove { slug, channels } => {
            let mut last = None;
            for r in &channels {
                let id = x.channel_id(r).await?;
                let p: PlaylistDetail = x.c.delete(&format!("/playlists/{}/items/{id}", enc(&slug))).await?;
                last = Some(p);
            }
            if let Some(p) = last {
                x.emit(&p, output::playlist);
            }
        }
        PlaylistCmd::Move {
            slug,
            channel,
            position,
        } => {
            let id = x.channel_id(&channel).await?;
            let p: PlaylistDetail =
                x.c.post(
                    &format!("/playlists/{}/items/{id}/move", enc(&slug)),
                    &serde_json::json!({ "position": position }),
                )
                .await?;
            x.emit(&p, output::playlist);
        }
        PlaylistCmd::Set { slug, channels } => {
            let mut ids = Vec::new();
            for r in &channels {
                ids.push(x.channel_id(r).await?);
            }
            let p: PlaylistDetail =
                x.c.put(
                    &format!("/playlists/{}/items", enc(&slug)),
                    &ItemsReplace { channel_ids: ids },
                )
                .await?;
            x.emit(&p, output::playlist);
        }
        PlaylistCmd::Url { slug } => {
            // Verify it exists so typos fail loudly.
            let _: PlaylistDetail = x.c.get(&format!("/playlists/{}", enc(&slug))).await?;
            let m3u = format!("{}/p/{}.m3u", x.c.base(), enc(&slug));
            x.emit(&serde_json::json!({ "m3u": m3u }), |_| println!("{m3u}"));
        }
    }
    Ok(())
}

enum AddOutcome {
    Created(Channel),
    WouldCreate(ChannelCreate, String),
}

async fn add_one(x: &Ctx, a: &crate::AddArgs, input: &str) -> R<AddOutcome> {
    let r: ResolveResponse = x.c.post("/resolve", &ResolveRequest { input: input.into() }).await?;
    if let Some(id) = r.duplicate_of {
        return Err(CliError::Api {
            code: "conflict".into(),
            message: format!("already exists as channel {id}"),
        });
    }
    let req = ChannelCreate {
        name: a.name.clone().unwrap_or_else(|| default_name(&r.source)),
        group_title: a.group.clone(),
        tvg_id: a.tvg_id.clone(),
        tvg_logo: a.logo.clone(),
        source: Some(r.source.clone()),
        input: None,
        is_event: a.event || a.kickoff.is_some(),
        kickoff_at: a.kickoff,
        enabled: !a.disabled,
        playlists: a.playlists.clone(),
    };
    if a.dry_run {
        return Ok(AddOutcome::WouldCreate(req, r.stream_url));
    }
    Ok(AddOutcome::Created(x.c.post("/channels", &req).await?))
}

use api_types::{
    Channel, ChannelCreate, ChannelInfo, ChannelPatch, CheckRequest, CheckResponse, EpgChannel, EpgFetchRequest,
    EpgFetchResult, EpgFetchStatus, EpgSource, EpgSourceCreate, EpgSourcePatch, Hook, HookConfig, HookCreate,
    HookDelivery, HookPatch, ImportReport, ImportRequest, ItemAdd, ItemsReplace, Playlist, PlaylistCreate,
    PlaylistDetail, PlaylistPatch, RefreshRequest, RefreshResponse, ResolveRequest, ResolveResponse, ResyncReport,
    Route, RouteConfig, RouteCreate, RoutePatch, RouteStatus, RouteTestRequest, RouteTestResult, Settings,
    SettingsPatch, Status,
};
use serde::Serialize;

use crate::client::{CliError, Client};
use crate::{Cli, Command, EpgCmd, EpgSourcesCmd, HookCmd, PlaylistCmd, RouteCmd, SettingsCmd, output};

type R<T = ()> = Result<T, CliError>;

const DEFAULT_SERVER: &str = "http://127.0.0.1:9300";

struct Ctx {
    c: Client,
    json: bool,
    quiet: bool,
    /// Lazily loaded channel list for resolving name references.
    channels: Option<Vec<Channel>>,
    /// Lazily loaded route list for resolving route references.
    routes: Option<Vec<Route>>,
}

impl Ctx {
    fn emit<T: Serialize>(&self, v: &T, human: impl FnOnce(&T)) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        } else {
            human(v);
        }
    }

    /// Resolve a channel reference: numeric id, exact name (case-insensitive), or a channel's exact
    /// stored source or identity (as `lineup show` prints them).
    async fn channel_id(&mut self, r: &str) -> R<i64> {
        if let Ok(id) = r.parse::<i64>() {
            return Ok(id);
        }
        if self.channels.is_none() {
            self.channels = Some(self.c.get("/channels").await?);
        }
        let all = self.channels.as_ref().unwrap();
        let by_name: Vec<&Channel> = all.iter().filter(|c| c.name.eq_ignore_ascii_case(r)).collect();
        let matches = if by_name.is_empty() {
            let t = r.trim();
            all.iter().filter(|c| c.source == t || c.identity == t).collect()
        } else {
            by_name
        };
        match matches.len() {
            1 => Ok(matches[0].id),
            0 => Err(CliError::Api {
                code: "not_found".into(),
                message: format!("no channel matches {r:?}"),
            }),
            n => {
                let ids: Vec<String> = matches.iter().map(|c| c.id.to_string()).collect();
                Err(CliError::Usage(format!(
                    "{r:?} matches {n} channels (ids {}); use an id",
                    ids.join(", ")
                )))
            }
        }
    }

    /// Resolve a route reference: numeric id or exact name (case-insensitive).
    async fn route(&mut self, r: &str) -> R<Route> {
        if self.routes.is_none() {
            self.routes = Some(self.c.get("/routes").await?);
        }
        let all = self.routes.as_ref().unwrap();
        let found = match r.trim().parse::<i64>() {
            Ok(id) => all.iter().find(|x| x.id == id),
            Err(_) => all.iter().find(|x| x.name.eq_ignore_ascii_case(r.trim())),
        };
        found.cloned().ok_or_else(|| CliError::Api {
            code: "not_found".into(),
            message: format!("no route matches {r:?} (see `lineup route ls`)"),
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

pub async fn run(cli: Cli) -> R {
    let mut x = Ctx {
        c: Client::new(&server_url(cli.server)?)?,
        json: cli.json,
        quiet: cli.quiet,
        channels: None,
        routes: None,
    };
    match cli.command {
        Command::Serve(_) | Command::Usage | Command::Completions { .. } => unreachable!("handled by caller"),

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

        Command::Resolve { input, route } => {
            let route_id = x.route(&route).await?.id;
            let r: ResolveResponse = x.c.post("/resolve", &ResolveRequest { route_id, input }).await?;
            x.emit(&r, |r| {
                output::validated(&r.validated);
                match (r.duplicate_of, &r.duplicate_source) {
                    (Some(id), Some(old)) => println!("duplicate:  yes, channel {id}, which has source {old}"),
                    (Some(id), None) => println!("duplicate:  yes, channel {id}"),
                    (None, _) => println!("duplicate:  no"),
                }
                output::suggestion(r);
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
            let route_id = x.route(&a.route).await?.id;
            for input in &a.inputs {
                match add_one(&x, &a, route_id, input).await {
                    Ok(AddOutcome::Created(ch)) => {
                        if !x.json {
                            if x.quiet {
                                println!("{}", ch.id);
                            } else {
                                println!("added {} {:?} -> {}", ch.id, ch.name, ch.play_url);
                            }
                        }
                        results.push(serde_json::json!({ "input": input, "ok": true, "channel": ch }));
                    }
                    Ok(AddOutcome::WouldCreate(req, url)) => {
                        if !x.json {
                            println!("would add {:?} ({}) -> {url}", req.name, req.input);
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
                route_id: Option<i64>,
                #[serde(skip_serializing_if = "Option::is_none")]
                health: Option<&'static str>,
            }
            let route_id = match &l.route {
                Some(r) => Some(x.route(r).await?.id),
                None => None,
            };
            let list: Vec<Channel> =
                x.c.get_query(
                    "/channels",
                    &Q {
                        q: l.query,
                        group: l.group,
                        playlist: l.playlist,
                        route_id,
                        health: l.health.map(|h| match h {
                            crate::HealthArg::Ok => "ok",
                            crate::HealthArg::Dead => "dead",
                        }),
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

        Command::Info { channel } => {
            let id = x.channel_id(&channel).await?;
            let info: ChannelInfo = x.c.get(&format!("/channels/{id}/info")).await?;
            x.emit(&info, output::channel_info);
        }

        Command::Edit(e) => {
            let id = x.channel_id(&e.channel).await?;
            let route_id = match &e.route {
                Some(r) => Some(x.route(r).await?.id),
                None => None,
            };
            let tri = |set: Option<String>, clear: bool| if clear { Some(None) } else { set.map(Some) };
            let patch = ChannelPatch {
                name: e.name,
                group_title: tri(e.group, e.no_group),
                tvg_id: tri(e.tvg_id, e.no_tvg_id),
                tvg_logo: tri(e.logo, e.no_logo),
                route_id,
                input: e.source,
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
            let route_id = x.route(&i.route).await?.id;
            let req = ImportRequest {
                m3u_text,
                url,
                playlist_slug: i.playlist,
                route_id,
                dry_run: i.dry_run,
            };
            let r: ImportReport = x.c.post("/import", &req).await?;
            x.emit(&r, output::import);
        }

        Command::Epg(cmd) => epg(&mut x, cmd).await?,
        Command::Hook(cmd) => hook(&mut x, cmd).await?,

        Command::Check { channels, force } => {
            let mut ids = Vec::new();
            for c in &channels {
                ids.push(x.channel_id(c).await?);
            }
            let r: CheckResponse = x.c.post_slow("/checks", &CheckRequest { ids, force }).await?;
            x.emit(&r, |r| {
                if r.background {
                    println!("checking all channels in the background; see `lineup ls` for results");
                }
                output::check_results(&r.results);
            });
        }

        Command::Refresh { slugs } => {
            let r: RefreshResponse = x.c.post("/refresh", &RefreshRequest { slugs }).await?;
            x.emit(&r, |r| println!("refreshed: {}", r.slugs.join(", ")));
        }

        Command::Route(cmd) => route(&mut x, cmd).await?,
        Command::Settings(SettingsCmd::Get) => {
            let s: Settings = x.c.get("/settings").await?;
            x.emit(&s, output::settings);
        }
        Command::Settings(SettingsCmd::Set { key, value }) => {
            let mut patch = SettingsPatch::default();
            match key.as_str() {
                "epg_refresh_hours" => {
                    patch.epg_refresh_hours = Some(
                        value
                            .parse()
                            .map_err(|_| CliError::Usage(format!("epg_refresh_hours: not a number: {value:?}")))?,
                    )
                }
                "check_interval_hours" => {
                    patch.check_interval_hours = Some(
                        value
                            .parse()
                            .map_err(|_| CliError::Usage(format!("check_interval_hours: not a number: {value:?}")))?,
                    )
                }
                other => return Err(CliError::Usage(format!("unknown setting {other:?}"))),
            }
            let s: Settings = x.c.patch("/settings", &patch).await?;
            x.emit(&s, output::settings);
        }
    }
    Ok(())
}

async fn route(x: &mut Ctx, cmd: RouteCmd) -> R {
    match cmd {
        RouteCmd::Ls => {
            let list: Vec<Route> = x.c.get("/routes").await?;
            if x.quiet && !x.json {
                for r in &list {
                    println!("{}", r.id);
                }
            } else {
                x.emit(&list, |l| output::routes(l));
            }
        }
        RouteCmd::Show { route } => {
            let r = x.route(&route).await?;
            let status: RouteStatus = x.c.get(&format!("/routes/{}/status", r.id)).await?;
            let both = serde_json::json!({ "route": r, "status": status });
            x.emit(&both, |_| output::route(&r, Some(&status)));
        }
        RouteCmd::Add {
            name,
            direct,
            gateway,
            rule,
            disabled,
        } => {
            let config = match (direct, gateway, rule) {
                (true, None, None) => RouteConfig::Direct {},
                (false, Some(base_url), Some(rule)) => RouteConfig::Gateway { base_url, rule },
                _ => return Err(CliError::Usage("pass --direct, or --gateway URL --rule RULE".into())),
            };
            let r: Route =
                x.c.post(
                    "/routes",
                    &RouteCreate {
                        name,
                        config,
                        enabled: !disabled,
                    },
                )
                .await?;
            x.emit(&r, |r| output::route(r, None));
        }
        RouteCmd::Edit {
            route,
            name,
            base_url,
            enable,
            disable,
        } => {
            let id = x.route(&route).await?.id;
            let patch = RoutePatch {
                name,
                enabled: if enable {
                    Some(true)
                } else if disable {
                    Some(false)
                } else {
                    None
                },
                base_url,
            };
            let r: Route = x.c.patch(&format!("/routes/{id}"), &patch).await?;
            x.emit(&r, |r| output::route(r, None));
        }
        RouteCmd::Rm { route } => {
            let id = x.route(&route).await?.id;
            let _: () = x.c.delete(&format!("/routes/{id}")).await?;
            x.emit(&serde_json::json!({ "deleted": id }), |_| {
                println!("deleted route {id}")
            });
        }
        RouteCmd::Test {
            route,
            gateway,
            rule,
            input,
        } => {
            let config = match (route, gateway) {
                (Some(r), None) => x.route(&r).await?.config,
                (None, Some(base_url)) => RouteConfig::Gateway {
                    base_url,
                    // Without a rule the test lists the gateway's rules and reports the empty one missing.
                    rule: rule.unwrap_or_else(|| "-".into()),
                },
                _ => return Err(CliError::Usage("pass a route, or --gateway URL [--rule RULE]".into())),
            };
            let r: RouteTestResult =
                x.c.post_slow("/routes/test", &RouteTestRequest { config, input })
                    .await?;
            x.emit(&r, output::route_test);
            if !r.ok {
                return Err(CliError::Reported(1));
            }
        }
        RouteCmd::Resync { route } => {
            let id = x.route(&route).await?.id;
            let r: ResyncReport =
                x.c.post_slow(&format!("/routes/{id}/resync"), &serde_json::json!({}))
                    .await?;
            x.emit(&r, output::resync);
        }
    }
    Ok(())
}

async fn epg(x: &mut Ctx, cmd: EpgCmd) -> R {
    match cmd {
        EpgCmd::Fetch { ids } => {
            let r: Vec<EpgFetchResult> =
                x.c.post_slow("/epg/fetch", &EpgFetchRequest { source_ids: ids })
                    .await?;
            x.emit(&r, |r| output::epg_fetch(r));
            if r.iter().any(|r| r.status == EpgFetchStatus::Error) {
                return Err(CliError::Reported(1));
            }
        }
        EpgCmd::Sources(EpgSourcesCmd::Ls) => {
            let l: Vec<EpgSource> = x.c.get("/epg/sources").await?;
            x.emit(&l, |l| output::epg_sources(l));
        }
        EpgCmd::Sources(EpgSourcesCmd::Add { url, name, priority }) => {
            let req = EpgSourceCreate {
                url,
                name,
                priority,
                enabled: true,
            };
            let s: EpgSource = x.c.post("/epg/sources", &req).await?;
            x.emit(&s, |s| println!("added EPG source {} ({}); fetching now", s.id, s.name));
        }
        EpgCmd::Sources(EpgSourcesCmd::Edit {
            id,
            name,
            url,
            priority,
            enable,
            disable,
        }) => {
            let patch = EpgSourcePatch {
                name,
                url,
                priority,
                enabled: if enable {
                    Some(true)
                } else if disable {
                    Some(false)
                } else {
                    None
                },
            };
            let s: EpgSource = x.c.patch(&format!("/epg/sources/{id}"), &patch).await?;
            x.emit(&s, |s| output::epg_sources(std::slice::from_ref(s)));
        }
        EpgCmd::Sources(EpgSourcesCmd::Rm { id }) => {
            let _: () = x.c.delete(&format!("/epg/sources/{id}")).await?;
            if x.json {
                println!("{}", serde_json::json!({ "deleted": id }));
            } else if !x.quiet {
                println!("deleted EPG source {id}");
            }
        }
        EpgCmd::Search { query, limit } => {
            let l: Vec<EpgChannel> =
                x.c.get_query("/epg/channels", &[("q", query), ("limit", limit.to_string())])
                    .await?;
            x.emit(&l, |l| output::epg_channels(l));
        }
    }
    Ok(())
}

async fn hook(x: &mut Ctx, cmd: HookCmd) -> R {
    match cmd {
        HookCmd::Ls => {
            let l: Vec<Hook> = x.c.get("/hooks").await?;
            x.emit(&l, |l| output::hooks(l));
        }
        HookCmd::Add {
            name,
            url,
            method,
            headers,
            common,
        } => {
            let req = HookCreate {
                name,
                config: HookConfig::Webhook {
                    method,
                    url,
                    headers: headers.into_iter().collect(),
                },
                events: common.events,
                debounce_secs: common.debounce,
                enabled: true,
            };
            let h: Hook = x.c.post("/hooks", &req).await?;
            x.emit(&h, |h| {
                println!("added hook {} ({}) for {}", h.id, h.name, h.events.join(", "))
            });
        }
        HookCmd::Edit {
            id,
            name,
            url,
            method,
            headers,
            common,
        } => {
            // The current config comes back with masked header values; sending a mask back keeps
            // the stored secret.
            let cur: Hook = x.c.get(&format!("/hooks/{id}")).await?;
            let HookConfig::Webhook {
                method: cur_method,
                url: cur_url,
                headers: cur_headers,
            } = cur.config;
            let config = (url.is_some() || method.is_some() || !headers.is_empty()).then(|| HookConfig::Webhook {
                method: method.unwrap_or(cur_method),
                url: url.unwrap_or(cur_url),
                headers: if headers.is_empty() {
                    cur_headers
                } else {
                    headers.into_iter().collect()
                },
            });
            let patch = HookPatch {
                name,
                config,
                events: common.events,
                debounce_secs: common.debounce,
                enabled: None,
            };
            let h: Hook = x.c.patch(&format!("/hooks/{id}"), &patch).await?;
            x.emit(&h, |h| println!("updated hook {} ({})", h.id, h.name));
        }
        HookCmd::Enable { id } | HookCmd::Disable { id } => {
            let enabled = matches!(cmd, HookCmd::Enable { .. });
            let patch = HookPatch {
                enabled: Some(enabled),
                ..Default::default()
            };
            let h: Hook = x.c.patch(&format!("/hooks/{id}"), &patch).await?;
            x.emit(&h, |h| {
                println!(
                    "{} hook {} ({})",
                    if enabled { "enabled" } else { "disabled" },
                    h.id,
                    h.name
                )
            });
        }
        HookCmd::Rm { id } => {
            let _: () = x.c.delete(&format!("/hooks/{id}")).await?;
            if x.json {
                println!("{}", serde_json::json!({ "deleted": id }));
            } else if !x.quiet {
                println!("deleted hook {id}");
            }
        }
        HookCmd::Test { id } => {
            let d: HookDelivery = x.c.post(&format!("/hooks/{id}/test"), &()).await?;
            x.emit(&d, |d| output::deliveries(std::slice::from_ref(d)));
            if !d.ok {
                return Err(CliError::Reported(1));
            }
        }
        HookCmd::Deliveries { id, limit } => {
            let l: Vec<HookDelivery> =
                x.c.get_query(&format!("/hooks/{id}/deliveries"), &[("limit", limit)])
                    .await?;
            x.emit(&l, |l| output::deliveries(l));
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

async fn add_one(x: &Ctx, a: &crate::AddArgs, route_id: i64, input: &str) -> R<AddOutcome> {
    let r: ResolveResponse =
        x.c.post_slow(
            "/resolve",
            &ResolveRequest {
                route_id,
                input: input.into(),
            },
        )
        .await?;
    if let Some(id) = r.duplicate_of {
        let hint = match &r.duplicate_source {
            Some(old) => format!(
                "; it has source {old}. To update it: lineup edit {id} --source {}",
                shell_word(input)
            ),
            None => String::new(),
        };
        return Err(CliError::Api {
            code: "conflict".into(),
            message: format!("already exists as channel {id}{hint}"),
        });
    }
    if !x.json {
        for w in &r.validated.warnings {
            eprintln!("warning: {input}: {w}");
        }
    }
    let s = &r.suggestion;
    let has_extra = s.group_title.is_some() || s.tvg_id.is_some() || s.tvg_logo.is_some();
    // Suggestions beyond the name are applied with --yes, or after confirming on a terminal.
    let accept = a.yes
        || (has_extra && !x.json && !x.quiet && is_interactive() && {
            println!("{input}:");
            output::suggestion(&r);
            confirm("Use these suggestions?")
        });
    if has_extra && !accept && !x.json && !x.quiet {
        eprintln!("note: {input}: suggestions not applied (pass --yes to accept them)");
    }
    let pick = |explicit: &Option<String>, suggested: &Option<String>| {
        explicit
            .clone()
            .or_else(|| if accept { suggested.clone() } else { None })
    };
    let req = ChannelCreate {
        name: a.name.clone().unwrap_or_else(|| s.name.clone()),
        group_title: pick(&a.group, &s.group_title),
        tvg_id: pick(&a.tvg_id, &s.tvg_id),
        tvg_logo: pick(&a.logo, &s.tvg_logo),
        route_id,
        input: input.to_string(),
        enabled: !a.disabled,
        playlists: a.playlists.clone(),
    };
    if a.dry_run {
        return Ok(AddOutcome::WouldCreate(req, r.validated.play_url));
    }
    Ok(AddOutcome::Created(x.c.post_slow("/channels", &req).await?))
}

/// Quote a word for a shell command line when it needs it.
fn shell_word(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Ask a yes/no question on the terminal; Enter means yes.
fn confirm(question: &str) -> bool {
    use std::io::Write;
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

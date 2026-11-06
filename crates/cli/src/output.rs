//! Human-readable rendering.

use api_types::{
    Channel, EpgChannel, EpgFetchResult, EpgFetchStatus, EpgSource, ImportAction, ImportReport, Playlist,
    PlaylistDetail, Settings, Source, Status,
};

pub fn source_label(s: &Source) -> String {
    match s {
        Source::Acestream { id } => format!("ace:{}", &id[..8.min(id.len())]),
        Source::Vk { owner_id, video_id, .. } => format!("vk:{owner_id}_{video_id}"),
        Source::Raw { url } => format!("raw:{}", truncate(url, 40)),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// Simple left-aligned table.
pub fn table(headers: &[&str], rows: Vec<Vec<String>>) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == cells.len() {
                    c.clone()
                } else {
                    format!("{c:<w$}", w = widths[i])
                }
            })
            .collect();
        println!("{}", parts.join("  ").trim_end());
    };
    line(headers.iter().map(|h| h.to_string()).collect());
    for r in rows {
        line(r);
    }
}

pub fn channels(list: &[Channel]) {
    if list.is_empty() {
        println!("(no channels)");
        return;
    }
    table(
        &["ID", "NAME", "GROUP", "SOURCE", "TVG-ID", "PLAYLISTS"],
        list.iter()
            .map(|c| {
                let name = if c.enabled {
                    c.name.clone()
                } else {
                    format!("{} (disabled)", c.name)
                };
                let name = if c.is_event { format!("{name} [event]") } else { name };
                vec![
                    c.id.to_string(),
                    name,
                    c.group_title.clone().unwrap_or_default(),
                    source_label(&c.source),
                    c.tvg_id.clone().unwrap_or_default(),
                    c.playlists.join(","),
                ]
            })
            .collect(),
    );
}

pub fn channel(c: &Channel) {
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    println!("id:         {}", c.id);
    println!("name:       {}", c.name);
    println!("group:      {}", opt(&c.group_title));
    println!("tvg-id:     {}", opt(&c.tvg_id));
    println!("logo:       {}", opt(&c.tvg_logo));
    println!("source:     {}", serde_json::to_string(&c.source).unwrap_or_default());
    println!("stream url: {}", c.stream_url);
    println!("enabled:    {}", c.enabled);
    if c.is_event {
        println!(
            "event:      yes, kickoff {}",
            c.kickoff_at.map(|k| k.to_rfc3339()).unwrap_or_else(|| "-".into())
        );
    }
    println!(
        "playlists:  {}",
        if c.playlists.is_empty() {
            "-".into()
        } else {
            c.playlists.join(", ")
        }
    );
}

pub fn playlists(list: &[Playlist]) {
    if list.is_empty() {
        println!("(no playlists)");
        return;
    }
    table(
        &["SLUG", "NAME", "CHANNELS"],
        list.iter()
            .map(|p| vec![p.slug.clone(), p.name.clone(), p.channel_count.to_string()])
            .collect(),
    );
}

pub fn playlist(p: &PlaylistDetail) {
    println!("{} ({}), {} channels", p.playlist.slug, p.playlist.name, p.items.len());
    if p.items.is_empty() {
        return;
    }
    table(
        &["CHNO", "ID", "NAME", "SOURCE"],
        p.items
            .iter()
            .map(|i| {
                let name = if i.channel.enabled {
                    i.channel.name.clone()
                } else {
                    format!("{} (disabled)", i.channel.name)
                };
                vec![
                    i.position.to_string(),
                    i.channel.id.to_string(),
                    name,
                    source_label(&i.channel.source),
                ]
            })
            .collect(),
    );
}

pub fn import(r: &ImportReport) {
    let verb = if r.dry_run { "would" } else { "did" };
    println!(
        "{} into {}{}",
        if r.dry_run { "Import preview" } else { "Imported" },
        r.playlist_slug,
        if r.playlist_created { " (new playlist)" } else { "" }
    );
    table(
        &["#", "ACTION", "ID", "NAME", "SOURCE", "DIFF"],
        r.entries
            .iter()
            .map(|e| {
                let action = match e.action {
                    ImportAction::Create => "create",
                    ImportAction::Reuse => "reuse",
                    ImportAction::SkipInPlaylist => "skip",
                };
                let diff = e
                    .diff
                    .iter()
                    .map(|d| {
                        format!(
                            "{}: {} -> {}",
                            d.field,
                            d.existing.as_deref().unwrap_or("-"),
                            d.incoming.as_deref().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                vec![
                    e.index.to_string(),
                    action.into(),
                    e.channel_id.map(|i| i.to_string()).unwrap_or_else(|| "-".into()),
                    e.name.clone(),
                    source_label(&e.source),
                    diff,
                ]
            })
            .collect(),
    );
    let count = |a| r.entries.iter().filter(|e| e.action == a).count();
    println!(
        "{verb} create {}, reuse {}, skip {}",
        count(ImportAction::Create),
        count(ImportAction::Reuse),
        count(ImportAction::SkipInPlaylist)
    );
    for s in &r.skipped_lines {
        println!("ignored: {s}");
    }
    if r.entries.iter().any(|e| !e.diff.is_empty()) {
        println!("note: existing channels keep their metadata; DIFF shows what the file has instead.");
    }
}

pub fn settings(s: &Settings) {
    println!("acexy_base:  {}", s.acexy_base);
    println!("engine_base: {}", s.engine_base);
    println!("vk_base:     {}", s.vk_base);
    println!("epg_refresh_hours: {}", s.epg_refresh_hours);
}

pub fn epg_sources(list: &[EpgSource]) {
    if list.is_empty() {
        println!("(no EPG sources)");
        return;
    }
    table(
        &["ID", "NAME", "PRIO", "CHANNELS", "LAST FETCH", "STATUS", "URL"],
        list.iter()
            .map(|s| {
                let name = if s.enabled {
                    s.name.clone()
                } else {
                    format!("{} (disabled)", s.name)
                };
                let status = match (&s.last_status, &s.last_error) {
                    (Some(st), Some(e)) if st == "error" => format!("error: {}", truncate(e, 60)),
                    (Some(st), _) => st.clone(),
                    (None, _) => "-".into(),
                };
                vec![
                    s.id.to_string(),
                    name,
                    s.priority.to_string(),
                    s.channel_count.to_string(),
                    s.last_fetch_at
                        .map_or("-".into(), |t| t.format("%Y-%m-%d %H:%M").to_string()),
                    status,
                    s.url.clone(),
                ]
            })
            .collect(),
    );
}

pub fn epg_fetch(list: &[EpgFetchResult]) {
    if list.is_empty() {
        println!("(no EPG sources)");
    }
    for r in list {
        match r.status {
            EpgFetchStatus::Updated => println!(
                "source {}: updated, {} channels, {} programmes",
                r.source_id,
                r.channels.unwrap_or(0),
                r.programmes.unwrap_or(0)
            ),
            EpgFetchStatus::NotModified => println!("source {}: not modified", r.source_id),
            EpgFetchStatus::Disabled => println!("source {}: disabled, skipped", r.source_id),
            EpgFetchStatus::Error => println!("source {}: error: {}", r.source_id, r.error.as_deref().unwrap_or("?")),
        }
    }
}

pub fn epg_channels(list: &[EpgChannel]) {
    if list.is_empty() {
        println!("(no matches)");
        return;
    }
    table(
        &["TVG-ID", "NAME", "SCORE", "SOURCE"],
        list.iter()
            .map(|c| {
                vec![
                    c.xmltv_id.clone(),
                    c.display_names.first().cloned().unwrap_or_default(),
                    c.score.map_or(String::new(), |s| format!("{s:.2}")),
                    c.source_id.to_string(),
                ]
            })
            .collect(),
    );
}

pub fn status(s: &Status, server: &str) {
    println!("server:  {server}");
    println!("version: {}", s.version);
}

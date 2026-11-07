-- lineup 0.4 schema. Databases of lineup 0.3 and older are not upgraded: the server refuses to
-- open them (their migration 0001 differs from this one).

-- App-wide settings; each value is JSON.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- One row per integration type; a blank required URL means "not configured".
CREATE TABLE integrations (
    kind       TEXT PRIMARY KEY CHECK (kind IN ('acestream', 'vk', 'direct')),
    config     TEXT    NOT NULL, -- JSON of api_types::IntegrationConfig
    revision   INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT    NOT NULL
);

INSERT INTO integrations (kind, config, updated_at) VALUES
    ('acestream', '{"kind":"acestream","engine_url":"","acexy_url":null,"probe_interval_hours":0}',
     strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ('vk', '{"kind":"vk","url":""}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ('direct', '{"kind":"direct"}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));

CREATE TABLE channels (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    group_title TEXT,
    tvg_id      TEXT,
    tvg_logo    TEXT,
    -- Also the channel's integration (one per kind).
    source_kind TEXT    NOT NULL CHECK (source_kind IN ('acestream', 'vk', 'direct')),
    source      TEXT    NOT NULL, -- JSON of api_types::Source
    source_key  TEXT    NOT NULL,
    is_event    INTEGER NOT NULL DEFAULT 0,
    kickoff_at  TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL,
    UNIQUE (source_kind, source_key)
);

CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    slug       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    channel_id  INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, channel_id),
    UNIQUE (playlist_id, position)
);

CREATE INDEX playlist_items_channel ON playlist_items (channel_id);

-- Latest probe attempt and last conclusive result per channel (no history).
CREATE TABLE channel_health (
    channel_id      INTEGER PRIMARY KEY REFERENCES channels (id) ON DELETE CASCADE,
    conclusive      TEXT CHECK (conclusive IN ('ok', 'dead')),
    conclusive_at   TEXT,
    last_attempt    TEXT NOT NULL CHECK (last_attempt IN ('ok', 'dead', 'unknown', 'skipped')),
    last_attempt_at TEXT NOT NULL,
    detail          TEXT -- JSON
);

-- AUTOINCREMENT: ids are never reused, so a fetch of a deleted source cannot land in a new one.
CREATE TABLE epg_sources (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    url           TEXT    NOT NULL,
    priority      INTEGER NOT NULL DEFAULT 0,
    enabled       INTEGER NOT NULL DEFAULT 1,
    current_gen   INTEGER,
    previous_gen  INTEGER,
    last_fetch_at TEXT,
    last_status   TEXT, -- ok | not_modified | error
    last_error    TEXT,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL
);

-- Channels of each source's published generation; used for matching and the tvg-id picker.
CREATE TABLE epg_channels (
    source_id     INTEGER NOT NULL REFERENCES epg_sources (id) ON DELETE CASCADE,
    xmltv_id      TEXT    NOT NULL,
    display_names TEXT    NOT NULL, -- JSON array of strings
    icon          TEXT,
    PRIMARY KEY (source_id, xmltv_id)
);

CREATE INDEX epg_channels_xmltv_id ON epg_channels (xmltv_id);

CREATE TABLE hooks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    kind          TEXT    NOT NULL CHECK (kind IN ('webhook')),
    config        TEXT    NOT NULL, -- JSON of api_types::HookConfig, secrets included
    events        TEXT    NOT NULL, -- JSON array of event types
    debounce_secs INTEGER NOT NULL DEFAULT 30,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT    NOT NULL,
    updated_at    TEXT    NOT NULL
);

-- Delivery log; only the most recent rows per hook are kept.
CREATE TABLE hook_deliveries (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    hook_id   INTEGER NOT NULL REFERENCES hooks (id) ON DELETE CASCADE,
    events    TEXT    NOT NULL, -- JSON array of the delivered event types
    trigger   TEXT    NOT NULL CHECK (trigger IN ('events', 'test')),
    attempt   INTEGER NOT NULL,
    ok        INTEGER NOT NULL,
    http_code INTEGER,
    error     TEXT,
    at        TEXT    NOT NULL
);

CREATE INDEX hook_deliveries_hook ON hook_deliveries (hook_id, id);

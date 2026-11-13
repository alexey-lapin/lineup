-- lineup 0.5 schema. Databases of lineup 0.4 and older are not upgraded: the server refuses to
-- open them (their migration 0001 differs from this one).

-- App-wide settings; each value is JSON.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Ways to play channels. `direct`: the channel's source URL is emitted as-is. `gateway`: a gateway
-- service (base_url) and one of its rules turn the source into a play URL.
CREATE TABLE routes (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    name                 TEXT    NOT NULL UNIQUE,
    kind                 TEXT    NOT NULL CHECK (kind IN ('direct', 'gateway')),
    base_url             TEXT,  -- gateway only
    rule                 TEXT,  -- gateway only; immutable
    enabled              INTEGER NOT NULL DEFAULT 1,
    revision             INTEGER NOT NULL DEFAULT 0,
    -- The gateway rule's revisions at the last resync in which every channel succeeded.
    synced_match_revision TEXT,
    synced_play_revision  TEXT,
    last_resync          TEXT, -- JSON of api_types::ResyncReport
    created_at           TEXT    NOT NULL,
    updated_at           TEXT    NOT NULL,
    CHECK ((kind = 'direct' AND base_url IS NULL AND rule IS NULL)
        OR (kind = 'gateway' AND base_url IS NOT NULL AND rule IS NOT NULL))
);

INSERT INTO routes (name, kind, created_at, updated_at) VALUES
    ('Direct', 'direct', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));

CREATE TABLE channels (
    id              INTEGER PRIMARY KEY AUTOINCREMENT, -- never reused: delayed writes can't hit a newer channel
    name            TEXT    NOT NULL,
    group_title     TEXT,
    tvg_id          TEXT,
    tvg_logo        TEXT,
    route_id        INTEGER NOT NULL REFERENCES routes (id) ON DELETE RESTRICT,
    source          TEXT    NOT NULL, -- canonical reference (the gateway's `ref`, or the direct URL)
    identity        TEXT    NOT NULL, -- de-duplication key within the route
    play            TEXT,             -- gateway play reference (relative to base_url, or absolute)
    play_revision   TEXT,             -- the rule's play_revision when `play` was obtained
    source_revision INTEGER NOT NULL DEFAULT 0, -- bumped on every change of route/source/identity/play
    enabled         INTEGER NOT NULL DEFAULT 1,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL,
    UNIQUE (route_id, identity)
);

CREATE INDEX channels_route ON channels (route_id);

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

-- Latest check attempt and last conclusive result per channel (no history).
CREATE TABLE channel_health (
    channel_id      INTEGER PRIMARY KEY REFERENCES channels (id) ON DELETE CASCADE,
    conclusive      TEXT CHECK (conclusive IN ('ok', 'dead')),
    conclusive_at   TEXT,
    last_attempt    TEXT NOT NULL CHECK (last_attempt IN ('ok', 'dead', 'unknown', 'skipped')),
    last_attempt_at TEXT NOT NULL,
    content_state   TEXT CHECK (content_state IN ('live', 'ended_processing', 'recording')),
    reason          TEXT,
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

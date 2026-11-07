CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL -- JSON
);

CREATE TABLE channels (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL,
    group_title TEXT,
    tvg_id      TEXT,
    tvg_logo    TEXT,
    source_kind TEXT    NOT NULL CHECK (source_kind IN ('acestream', 'vk', 'raw')),
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

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

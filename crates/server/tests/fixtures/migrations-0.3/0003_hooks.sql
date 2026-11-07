CREATE TABLE hooks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    kind          TEXT    NOT NULL CHECK (kind IN ('jellyfin', 'webhook')),
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
    trigger   TEXT    NOT NULL CHECK (trigger IN ('events', 'followup', 'test')),
    attempt   INTEGER NOT NULL,
    ok        INTEGER NOT NULL,
    http_code INTEGER,
    error     TEXT,
    at        TEXT    NOT NULL
);

CREATE INDEX hook_deliveries_hook ON hook_deliveries (hook_id, id);

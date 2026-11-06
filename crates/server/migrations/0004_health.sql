CREATE TABLE channel_health (
    channel_id      INTEGER PRIMARY KEY REFERENCES channels (id) ON DELETE CASCADE,
    conclusive      TEXT CHECK (conclusive IN ('ok', 'dead')), -- last conclusive result
    conclusive_at   TEXT,
    last_attempt    TEXT NOT NULL CHECK (last_attempt IN ('ok', 'dead', 'unknown', 'skipped')),
    last_attempt_at TEXT NOT NULL,
    detail          TEXT -- JSON
);

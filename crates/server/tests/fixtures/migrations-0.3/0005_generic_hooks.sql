-- Hooks are generic webhooks only (0.3.0). A Jellyfin hook becomes the webhook it amounted to:
-- POST {base_url}/ScheduledTasks/Running/{task_id} with its API key in the Authorization header.
-- Without a stored task id the URL gets a placeholder and the hook is disabled until the owner
-- fixes the URL (lineup warns at startup). The follow-up resend is gone.
UPDATE hooks
SET kind = 'webhook',
    enabled = CASE WHEN trim(coalesce(json_extract(config, '$.task_id'), '')) = '' THEN 0 ELSE enabled END,
    config = json_object(
        'kind', 'webhook',
        'method', 'POST',
        'url', rtrim(json_extract(config, '$.base_url'), '/') || '/ScheduledTasks/Running/' ||
            CASE WHEN trim(coalesce(json_extract(config, '$.task_id'), '')) = ''
                 THEN 'REFRESH_GUIDE_TASK_ID'
                 ELSE trim(json_extract(config, '$.task_id')) END,
        'headers', json_object('Authorization', 'MediaBrowser Token="' || json_extract(config, '$.api_key') || '"')
    )
WHERE kind = 'jellyfin';

-- Deliveries no longer have a follow-up trigger; rebuild the table for the new CHECK, keeping the
-- remaining rows, their ids, the foreign key and the index.
CREATE TABLE hook_deliveries_new (
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
INSERT INTO hook_deliveries_new (id, hook_id, events, trigger, attempt, ok, http_code, error, at)
SELECT id, hook_id, events, trigger, attempt, ok, http_code, error, at
FROM hook_deliveries
WHERE trigger <> 'followup';
DROP TABLE hook_deliveries;
ALTER TABLE hook_deliveries_new RENAME TO hook_deliveries;
CREATE INDEX hook_deliveries_hook ON hook_deliveries (hook_id, id);

-- Derived positional index and current head for incremental EventLog refreshes.
-- qx_event_logs.content remains the canonical compatibility representation.
CREATE TABLE IF NOT EXISTS qx_event_log_entries (
    name TEXT NOT NULL,
    position BIGINT NOT NULL CHECK (position >= 0),
    event_json TEXT NOT NULL,
    PRIMARY KEY (name, position)
);

CREATE TABLE IF NOT EXISTS qx_event_log_heads (
    name TEXT PRIMARY KEY NOT NULL,
    event_count TEXT NOT NULL CHECK (event_count ~ '^[0-9]+$'),
    event_digest TEXT NOT NULL CHECK (event_digest ~ '^[0-9]+$'),
    next_seq TEXT NOT NULL CHECK (next_seq ~ '^[0-9]+$')
);

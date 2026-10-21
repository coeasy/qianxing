CREATE TABLE IF NOT EXISTS qx_audit_entries (
    sequence INTEGER PRIMARY KEY NOT NULL,
    record_json TEXT NOT NULL,
    previous_hash TEXT NOT NULL,
    entry_hash TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS qx_jobs (
    run_id TEXT PRIMARY KEY NOT NULL,
    envelope_json TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS qx_job_leases (
    run_id TEXT PRIMARY KEY NOT NULL,
    owner TEXT NOT NULL,
    expires_ts TEXT NOT NULL,
    fencing_token TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_control_state (
    state_id INTEGER PRIMARY KEY NOT NULL CHECK (state_id = 1),
    content TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_control_commands (
    command_id TEXT PRIMARY KEY NOT NULL,
    command_json TEXT NOT NULL,
    enqueued_ts TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS qx_control_command_leases (
    command_id TEXT PRIMARY KEY NOT NULL,
    owner TEXT NOT NULL,
    expires_ts TEXT NOT NULL,
    fencing_token TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_outbox_events (
    event_id TEXT PRIMARY KEY NOT NULL,
    topic TEXT NOT NULL,
    partition_key TEXT NOT NULL,
    sequence TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    trace_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_ts TEXT NOT NULL,
    attempts TEXT NOT NULL DEFAULT '0'
);
CREATE TABLE IF NOT EXISTS qx_outbox_leases (
    event_id TEXT PRIMARY KEY NOT NULL,
    owner TEXT NOT NULL,
    expires_ts TEXT NOT NULL,
    fencing_token TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_consumer_checkpoints (
    group_id TEXT NOT NULL,
    topic TEXT NOT NULL,
    partition_key TEXT NOT NULL,
    offset TEXT NOT NULL,
    event_id TEXT NOT NULL,
    updated_ts TEXT NOT NULL,
    PRIMARY KEY(group_id, topic, partition_key)
);
CREATE TABLE IF NOT EXISTS qx_consumer_processed (
    group_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    PRIMARY KEY(group_id, event_id)
);
CREATE TABLE IF NOT EXISTS qx_consumer_dead_letters (
    group_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(group_id, event_id, attempts)
);
CREATE TABLE IF NOT EXISTS qx_consumer_projections (
    group_id TEXT NOT NULL,
    projection_key TEXT NOT NULL,
    topic TEXT NOT NULL,
    partition_key TEXT NOT NULL,
    offset TEXT NOT NULL,
    event_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    updated_ts TEXT NOT NULL,
    PRIMARY KEY(group_id, projection_key)
);
CREATE TABLE IF NOT EXISTS qx_event_log_entries (
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    event_ts TEXT NOT NULL,
    prio INTEGER NOT NULL,
    dedup_key TEXT NOT NULL DEFAULT '',
    event_json TEXT NOT NULL,
    PRIMARY KEY (name, position)
);
CREATE UNIQUE INDEX IF NOT EXISTS qx_event_log_entries_seq
    ON qx_event_log_entries(name, seq);
CREATE UNIQUE INDEX IF NOT EXISTS qx_event_log_entries_dedup
    ON qx_event_log_entries(name, dedup_key) WHERE dedup_key <> '';
CREATE TABLE IF NOT EXISTS qx_event_log_state (
    name TEXT PRIMARY KEY NOT NULL,
    event_count INTEGER NOT NULL,
    next_seq TEXT NOT NULL,
    digest TEXT NOT NULL
);

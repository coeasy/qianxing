CREATE TABLE IF NOT EXISTS qx_event_logs (
    name TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_audit_entries (
    sequence TEXT PRIMARY KEY NOT NULL,
    record_json TEXT NOT NULL,
    previous_hash TEXT NOT NULL,
    entry_hash TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS qx_jobs (
    run_id TEXT PRIMARY KEY NOT NULL,
    envelope_json TEXT NOT NULL,
    done BOOLEAN NOT NULL DEFAULT FALSE
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
    done BOOLEAN NOT NULL DEFAULT FALSE
);
CREATE TABLE IF NOT EXISTS qx_control_command_leases (
    command_id TEXT PRIMARY KEY NOT NULL,
    owner TEXT NOT NULL,
    expires_ts TEXT NOT NULL,
    fencing_token TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS qx_snapshots (
    snapshot_id TEXT NOT NULL,
    state_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    PRIMARY KEY(snapshot_id, state_hash)
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
    "offset" TEXT NOT NULL,
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
    attempts TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(group_id, event_id, attempts)
);
CREATE TABLE IF NOT EXISTS qx_consumer_projections (
    group_id TEXT NOT NULL,
    projection_key TEXT NOT NULL,
    topic TEXT NOT NULL,
    partition_key TEXT NOT NULL,
    "offset" TEXT NOT NULL,
    event_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    updated_ts TEXT NOT NULL,
    PRIMARY KEY(group_id, projection_key)
);

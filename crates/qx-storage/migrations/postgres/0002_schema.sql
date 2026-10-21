CREATE INDEX IF NOT EXISTS qx_jobs_pending_idx
    ON qx_jobs(done, run_id);
CREATE INDEX IF NOT EXISTS qx_control_commands_pending_idx
    ON qx_control_commands(done, enqueued_ts, command_id);
CREATE INDEX IF NOT EXISTS qx_outbox_pending_idx
    ON qx_outbox_events(created_ts, sequence, event_id);

CREATE TABLE IF NOT EXISTS qx_json_state (
    state_key TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL CHECK (jsonb_typeof(content::jsonb) IS NOT NULL)
);

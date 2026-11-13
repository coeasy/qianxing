CREATE TABLE IF NOT EXISTS qx_deployment_blobs (
    sha256 TEXT PRIMARY KEY NOT NULL
        CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    content BYTEA NOT NULL
        CHECK (octet_length(content) BETWEEN 1 AND 268435456)
);

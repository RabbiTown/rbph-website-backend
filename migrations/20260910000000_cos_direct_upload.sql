-- Durable browser upload sessions. JSON stores the bounded, immutable file manifest
-- and per-file COS state; advisory locks serialize control requests across nodes.
CREATE TABLE rb_asset_upload (
    id TEXT PRIMARY KEY,
    owner_id INT REFERENCES rb_user(id) ON DELETE SET NULL,
    request_id TEXT NOT NULL,
    game_id INT REFERENCES rb_game(id) ON DELETE SET NULL,
    state SMALLINT NOT NULL DEFAULT 0 CHECK (state BETWEEN 0 AND 5),
    document JSONB NOT NULL,
    result JSONB,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    touched_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP + INTERVAL '24 hours',
    UNIQUE(owner_id, request_id)
);

CREATE INDEX rb_asset_upload_work ON rb_asset_upload(state, expires_at);

-- Keep legacy tables/queries wire compatible. Metadata is additive and cascades
-- with the asset group; absence means the historical content-v1/server digest.
CREATE TABLE rb_asset_digest (
    group_id INT PRIMARY KEY REFERENCES rb_asset_group(id) ON DELETE CASCADE,
    digest_version TEXT NOT NULL,
    sha256_source TEXT NOT NULL
);

CREATE TABLE rb_asset_upload_garbage (
    id BIGSERIAL PRIMARY KEY,
    backend TEXT NOT NULL,
    object_key TEXT NOT NULL,
    paths JSONB NOT NULL
);

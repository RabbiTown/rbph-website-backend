DROP TABLE rb_puzzle_store_index;
DROP TABLE rb_puzzle_store_doc;
DROP TABLE rb_puzzle_kv;

CREATE TABLE rb_puzzle_kv (
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    scope_type      SMALLINT NOT NULL,
    team_id         INT REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    key             TEXT NOT NULL,
    value           JSONB NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (scope_type = 0 AND team_id IS NULL AND puzzle_id IS NULL)
        OR (scope_type = 1 AND team_id IS NOT NULL AND puzzle_id IS NULL)
        OR (scope_type = 2 AND team_id IS NULL AND puzzle_id IS NOT NULL)
        OR (scope_type = 3 AND team_id IS NOT NULL AND puzzle_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX rb_idx_puzzle_kv_key
ON rb_puzzle_kv(game_id, scope_type, team_id, puzzle_id, key) NULLS NOT DISTINCT;

CREATE INDEX rb_idx_puzzle_kv_scope
ON rb_puzzle_kv(game_id, scope_type, team_id, puzzle_id, key);

ALTER TABLE rb_puzzle_kv ADD CONSTRAINT rb_ck_puzzle_kv_key
CHECK (length(key) > 0 AND length(key) <= 255);

CREATE TABLE rb_puzzle_store_doc (
    id              BIGSERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    scope_type      SMALLINT NOT NULL,
    team_id         INT REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    collection      TEXT NOT NULL,
    created_by      INT REFERENCES rb_user(id) ON DELETE SET NULL,
    value           JSONB NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (scope_type = 0 AND team_id IS NULL AND puzzle_id IS NULL)
        OR (scope_type = 1 AND team_id IS NOT NULL AND puzzle_id IS NULL)
        OR (scope_type = 2 AND team_id IS NULL AND puzzle_id IS NOT NULL)
        OR (scope_type = 3 AND team_id IS NOT NULL AND puzzle_id IS NOT NULL)
    )
);

ALTER TABLE rb_puzzle_store_doc ADD CONSTRAINT rb_ck_puzzle_store_doc_collection
CHECK (length(collection) > 0 AND length(collection) <= 64);

CREATE INDEX rb_idx_puzzle_store_doc_collection_id
ON rb_puzzle_store_doc(game_id, scope_type, team_id, puzzle_id, collection, id DESC);

CREATE INDEX rb_idx_puzzle_store_doc_collection_ctime
ON rb_puzzle_store_doc(game_id, scope_type, team_id, puzzle_id, collection, ctime_at DESC, id DESC);

CREATE TABLE rb_puzzle_store_index (
    doc_id          BIGINT NOT NULL REFERENCES rb_puzzle_store_doc(id) ON DELETE CASCADE,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    collection      TEXT NOT NULL,
    key             TEXT NOT NULL,
    value_text      TEXT,
    value_number    DOUBLE PRECISION,
    value_bool      BOOLEAN,
    PRIMARY KEY (doc_id, key)
);

ALTER TABLE rb_puzzle_store_index ADD CONSTRAINT rb_ck_puzzle_store_index_key
CHECK (length(key) > 0 AND length(key) <= 64);

ALTER TABLE rb_puzzle_store_index ADD CONSTRAINT rb_ck_puzzle_store_index_one_value
CHECK (
    (value_text IS NOT NULL)::INT
    + (value_number IS NOT NULL)::INT
    + (value_bool IS NOT NULL)::INT = 1
);

CREATE INDEX rb_idx_puzzle_store_index_text
ON rb_puzzle_store_index(game_id, collection, key, value_text, doc_id DESC)
WHERE value_text IS NOT NULL;

CREATE INDEX rb_idx_puzzle_store_index_number
ON rb_puzzle_store_index(game_id, collection, key, value_number, doc_id DESC)
WHERE value_number IS NOT NULL;

CREATE INDEX rb_idx_puzzle_store_index_bool
ON rb_puzzle_store_index(game_id, collection, key, value_bool, doc_id DESC)
WHERE value_bool IS NOT NULL;

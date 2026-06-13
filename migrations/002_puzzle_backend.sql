-- puzzle backend

CREATE TABLE rb_puzzle_backend (
    puzzle_id       INT PRIMARY KEY REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    enabled         BOOLEAN NOT NULL DEFAULT FALSE,
    source          TEXT NOT NULL DEFAULT '',
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE rb_puzzle_backend ADD CONSTRAINT rb_ck_puzzle_backend_source_not_blank
CHECK (NOT enabled OR length(btrim(source)) > 0);

CREATE TABLE rb_puzzle_kv (
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    team_id         INT REFERENCES rb_team(id) ON DELETE CASCADE,
    key             TEXT NOT NULL,
    value           JSONB NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX rb_idx_puzzle_kv_global_key
ON rb_puzzle_kv(puzzle_id, key)
WHERE team_id IS NULL;

CREATE UNIQUE INDEX rb_idx_puzzle_kv_team_key
ON rb_puzzle_kv(puzzle_id, team_id, key)
WHERE team_id IS NOT NULL;

CREATE INDEX rb_idx_puzzle_kv_puzzle_team
ON rb_puzzle_kv(puzzle_id, team_id, key);

ALTER TABLE rb_puzzle_kv ADD CONSTRAINT rb_ck_puzzle_kv_key
CHECK (length(key) > 0 AND length(key) <= 255);

CREATE TABLE rb_puzzle_store_doc (
    id              BIGSERIAL PRIMARY KEY,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    collection      TEXT NOT NULL,
    team_id         INT REFERENCES rb_team(id) ON DELETE SET NULL,
    user_id         INT REFERENCES rb_user(id) ON DELETE SET NULL,
    value           JSONB NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE rb_puzzle_store_doc ADD CONSTRAINT rb_ck_puzzle_store_doc_collection
CHECK (length(collection) > 0 AND length(collection) <= 64);

CREATE INDEX rb_idx_puzzle_store_doc_collection_id
ON rb_puzzle_store_doc(puzzle_id, collection, id DESC);

CREATE INDEX rb_idx_puzzle_store_doc_collection_ctime
ON rb_puzzle_store_doc(puzzle_id, collection, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_puzzle_store_doc_collection_team_ctime
ON rb_puzzle_store_doc(puzzle_id, collection, team_id, ctime_at DESC, id DESC);

CREATE TABLE rb_puzzle_store_index (
    doc_id          BIGINT NOT NULL REFERENCES rb_puzzle_store_doc(id) ON DELETE CASCADE,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
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
ON rb_puzzle_store_index(puzzle_id, collection, key, value_text, doc_id DESC)
WHERE value_text IS NOT NULL;

CREATE INDEX rb_idx_puzzle_store_index_number
ON rb_puzzle_store_index(puzzle_id, collection, key, value_number, doc_id DESC)
WHERE value_number IS NOT NULL;

CREATE INDEX rb_idx_puzzle_store_index_bool
ON rb_puzzle_store_index(puzzle_id, collection, key, value_bool, doc_id DESC)
WHERE value_bool IS NOT NULL;

CREATE TABLE rb_puzzle_backend_call_log (
    id              BIGSERIAL PRIMARY KEY,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    team_id         INT REFERENCES rb_team(id) ON DELETE SET NULL,
    user_id         INT REFERENCES rb_user(id) ON DELETE SET NULL,
    function_name   VARCHAR(64) NOT NULL,
    ok              BOOLEAN NOT NULL,
    error           TEXT,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX rb_idx_puzzle_backend_call_log_puzzle_ctime
ON rb_puzzle_backend_call_log(puzzle_id, ctime_at DESC);

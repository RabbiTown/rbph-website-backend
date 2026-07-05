ALTER TABLE rb_team
ADD COLUMN content_blocks_dirty BOOLEAN NOT NULL DEFAULT TRUE;

CREATE TABLE rb_content_block (
    id              SERIAL PRIMARY KEY,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    round_id        INT REFERENCES rb_round(id) ON DELETE CASCADE,
    sort            INT NOT NULL DEFAULT 0,
    name            VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL DEFAULT '',
    content_type    SMALLINT NOT NULL DEFAULT 0,
    visibility_cond TEXT NOT NULL DEFAULT 'default',
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK ((puzzle_id IS NOT NULL)::INT + (round_id IS NOT NULL)::INT = 1),
    CHECK (content_type IN (0, 1, 2)),
    CHECK (char_length(name) BETWEEN 1 AND 120)
);

CREATE INDEX rb_idx_content_block_puzzle_sort
ON rb_content_block(puzzle_id, sort, id)
WHERE puzzle_id IS NOT NULL;

CREATE INDEX rb_idx_content_block_round_sort
ON rb_content_block(round_id, sort, id)
WHERE round_id IS NOT NULL;

CREATE TABLE rb_team_content_block_unlock (
    team_id          INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    content_block_id INT NOT NULL REFERENCES rb_content_block(id) ON DELETE CASCADE,
    ctime_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, content_block_id)
);

CREATE TABLE rb_team_puzzle_trigger (
    team_id             INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id           INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    trigger_key         VARCHAR(64) NOT NULL,
    source_submission_id INT REFERENCES rb_submission(id) ON DELETE SET NULL,
    ctime_at            TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, puzzle_id, trigger_key),
    CHECK (trigger_key ~ '^[A-Za-z][A-Za-z0-9_-]{0,63}$')
);

INSERT INTO rb_content_block (puzzle_id, sort, name, content, content_type)
SELECT id, 0, 'Default', content, content_type
FROM rb_puzzle;

INSERT INTO rb_content_block (round_id, sort, name, content, content_type)
SELECT id, 0, 'Default', content, content_type
FROM rb_round;

ALTER TABLE rb_puzzle DROP COLUMN content, DROP COLUMN content_type;
ALTER TABLE rb_round DROP COLUMN content, DROP COLUMN content_type;

ALTER TABLE rb_puzzle
ALTER COLUMN unlock_cond DROP NOT NULL;

UPDATE rb_puzzle
SET unlock_cond = NULL
WHERE unlock_cond = 'default';

ALTER TABLE rb_content_block
ALTER COLUMN visibility_cond DROP NOT NULL,
ALTER COLUMN visibility_cond DROP DEFAULT;

UPDATE rb_content_block
SET visibility_cond = NULL
WHERE visibility_cond = 'default';

ALTER TABLE rb_hint
ADD COLUMN enable_cond TEXT,
ADD COLUMN cooldown_after_enable BOOLEAN NOT NULL DEFAULT FALSE,
ADD CONSTRAINT rb_ck_hint_cooldown_origin
CHECK (enable_cond IS NOT NULL OR NOT cooldown_after_enable);

CREATE TABLE rb_team_hint_enable (
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    hint_id         INT NOT NULL REFERENCES rb_hint(id) ON DELETE CASCADE,
    enabled_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, hint_id)
);

CREATE INDEX rb_idx_team_hint_enable_hint
ON rb_team_hint_enable(hint_id, team_id);

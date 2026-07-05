ALTER TABLE rb_puzzle
ADD COLUMN immediate_release_at TIMESTAMPTZ;

ALTER TABLE rb_puzzle
ADD CONSTRAINT rb_ck_puzzle_release_source
CHECK (release_phase_id IS NULL OR immediate_release_at IS NULL);

ALTER TABLE rb_release_event
ADD COLUMN game_id INT REFERENCES rb_game(id) ON DELETE CASCADE,
ADD COLUMN event_type SMALLINT NOT NULL DEFAULT 0;

UPDATE rb_release_event re
SET game_id = rp.game_id
FROM rb_release_phase rp
WHERE re.phase_id = rp.id;

ALTER TABLE rb_release_event
ALTER COLUMN game_id SET NOT NULL,
ALTER COLUMN phase_id DROP NOT NULL;

ALTER TABLE rb_release_event
ADD CONSTRAINT rb_ck_release_event_source
CHECK (
    (event_type = 0 AND phase_id IS NOT NULL) OR
    (event_type = 1 AND phase_id IS NULL)
);

CREATE TABLE rb_release_event_puzzle (
    event_id        BIGINT NOT NULL REFERENCES rb_release_event(id) ON DELETE CASCADE,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    PRIMARY KEY (event_id, puzzle_id)
);

CREATE INDEX rb_idx_release_event_game
ON rb_release_event(game_id, id);

CREATE INDEX rb_idx_release_event_puzzle_puzzle
ON rb_release_event_puzzle(puzzle_id, event_id);

CREATE TABLE rb_release_event_puzzle_team (
    event_id        BIGINT NOT NULL,
    puzzle_id       INT NOT NULL,
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    PRIMARY KEY (event_id, puzzle_id, team_id),
    FOREIGN KEY (event_id, puzzle_id)
        REFERENCES rb_release_event_puzzle(event_id, puzzle_id) ON DELETE CASCADE
);

CREATE INDEX rb_idx_release_event_puzzle_team_lookup
ON rb_release_event_puzzle_team(team_id, event_id);

CREATE VIEW rb_puzzle_effective_release AS
SELECT p.id AS puzzle_id,
    COALESCE(p.immediate_release_at, rp.release_at) AS release_at
FROM rb_puzzle p
LEFT JOIN rb_release_phase rp ON rp.id = p.release_phase_id
WHERE p.immediate_release_at IS NOT NULL OR rp.id IS NOT NULL;

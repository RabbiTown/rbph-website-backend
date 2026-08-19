ALTER TABLE rb_system_settings
ADD COLUMN leaderboard_refresh_interval_seconds INT NOT NULL DEFAULT 5;

CREATE SEQUENCE rb_leaderboard_dirty_revision_seq AS BIGINT;

CREATE TABLE rb_leaderboard_refresh_state (
    game_id       INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    board_type    TEXT NOT NULL DEFAULT 'main',
    next_version  BIGINT NOT NULL DEFAULT 0,
    full_rebuild  BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (game_id, board_type)
);

CREATE TABLE rb_leaderboard_dirty_team (
    game_id       INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    board_type    TEXT NOT NULL DEFAULT 'main',
    team_id       INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    revision      BIGINT NOT NULL DEFAULT nextval('rb_leaderboard_dirty_revision_seq'),
    affects_order BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (game_id, board_type, team_id)
);

CREATE INDEX rb_idx_leaderboard_dirty_game_revision
ON rb_leaderboard_dirty_team(game_id, board_type, revision);

CREATE INDEX rb_idx_team_puzzle_solved_team_solve_at
ON rb_team_puzzle(team_id, solve_at)
WHERE state = 1;

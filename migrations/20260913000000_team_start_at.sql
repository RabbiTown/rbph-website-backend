ALTER TABLE rb_team
ADD COLUMN start_at TIMESTAMPTZ;

UPDATE rb_team t
SET start_at = started.start_at
FROM (
    SELECT team_id, MIN(ctime_at) AS start_at
    FROM rb_submission
    WHERE saction = 3
    GROUP BY team_id
) started
WHERE t.id = started.team_id;

DROP INDEX rb_idx_team_game_flags_finish;

CREATE INDEX rb_idx_team_game_flags_finish
ON rb_team(game_id, is_banned, is_locked, start_at, finish_at);

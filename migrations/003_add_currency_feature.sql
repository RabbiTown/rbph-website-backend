ALTER TABLE rb_release_phase_feature_change
DROP CONSTRAINT rb_ck_release_phase_feature_change;

ALTER TABLE rb_release_phase_feature_change
ADD CONSTRAINT rb_ck_release_phase_feature_change
CHECK (
    (feature_type = 0 AND target_state IN (0, 1)) OR
    (feature_type IN (1, 2) AND target_state IN (0, 1, 2)) OR
    (feature_type IN (3, 4) AND target_state IN (0, 1))
);

ALTER TABLE rb_game_feature
DROP CONSTRAINT rb_ck_game_feature_state;

ALTER TABLE rb_game_feature
ADD CONSTRAINT rb_ck_game_feature_state
CHECK (
    (feature_type = 0 AND state IN (0, 1)) OR
    (feature_type IN (1, 2) AND state IN (0, 1, 2)) OR
    (feature_type IN (3, 4) AND state IN (0, 1))
);

UPDATE rb_team_currency tc
SET amount = GREATEST(
        0::NUMERIC,
        LEAST(
            tc.amount::NUMERIC
                + GREATEST(
                    FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60),
                    0::NUMERIC
                )
                    * (c.growth + tc.growth)::NUMERIC,
            c.max_amount::NUMERIC
        )
    )::BIGINT,
    utime_at = NOW()
FROM rb_currency c
WHERE tc.currency_id = c.id;

DELETE FROM rb_team_currency tc
WHERE NOT EXISTS (
    SELECT 1
    FROM rb_submission s
    WHERE s.team_id = tc.team_id AND s.saction = 3
);

INSERT INTO rb_game_feature (game_id, feature_type, state)
SELECT id, 4, 0
FROM rb_game
ON CONFLICT (game_id, feature_type) DO NOTHING;

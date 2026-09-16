UPDATE rb_game
SET settings = settings || jsonb_build_object(
    'team',
    CASE
        WHEN jsonb_typeof(settings -> 'team') = 'object' THEN settings -> 'team'
        ELSE '{}'::JSONB
    END
        || jsonb_build_object('allow_duplicate_names', TRUE)
);

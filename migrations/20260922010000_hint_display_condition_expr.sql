ALTER TABLE rb_hint
DROP CONSTRAINT rb_ck_hint_title_display_condition,
DROP CONSTRAINT rb_ck_hint_display_condition,
ALTER COLUMN title_display_condition DROP DEFAULT,
ALTER COLUMN display_condition DROP DEFAULT;

ALTER TABLE rb_hint
ALTER COLUMN title_display_condition TYPE TEXT USING CASE title_display_condition
    WHEN 0 THEN '(true)'
    WHEN 1 THEN '(hint-enabled)'
    WHEN 2 THEN '(hint-cooled-down)'
    WHEN 3 THEN '(and (hint-enabled) (hint-cooled-down))'
END,
ALTER COLUMN display_condition TYPE TEXT USING CASE display_condition
    WHEN 0 THEN '(true)'
    WHEN 1 THEN '(hint-enabled)'
    WHEN 2 THEN '(hint-cooled-down)'
    WHEN 3 THEN '(and (hint-enabled) (hint-cooled-down))'
END;

ALTER TABLE rb_hint
ALTER COLUMN title_display_condition DROP NOT NULL,
ALTER COLUMN display_condition DROP NOT NULL;

UPDATE rb_hint
SET title_display_condition = NULL
WHERE title_display_condition = '(hint-cooled-down)';

UPDATE rb_hint
SET display_condition = NULL
WHERE display_condition = '(true)';

ALTER TABLE rb_hint
DROP CONSTRAINT rb_ck_hint_cooldown_origin,
ADD COLUMN cooldown_origin SMALLINT NOT NULL DEFAULT 0;

UPDATE rb_hint
SET cooldown_origin = CASE WHEN cooldown_after_enable THEN 1 ELSE 0 END;

ALTER TABLE rb_hint
DROP COLUMN cooldown_after_enable,
ADD CONSTRAINT rb_ck_hint_cooldown_origin CHECK (cooldown_origin BETWEEN 0 AND 3);

CREATE TABLE rb_team_hint_visibility (
    team_id             INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    hint_id             INT NOT NULL REFERENCES rb_hint(id) ON DELETE CASCADE,
    displayed_at        TIMESTAMPTZ,
    title_displayed_at  TIMESTAMPTZ,
    PRIMARY KEY (team_id, hint_id),
    CHECK (displayed_at IS NOT NULL OR title_displayed_at IS NOT NULL)
);

CREATE INDEX rb_idx_team_hint_visibility_hint
ON rb_team_hint_visibility(hint_id, team_id);

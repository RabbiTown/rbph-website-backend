ALTER TABLE rb_hint
ADD COLUMN title_display_condition SMALLINT NOT NULL DEFAULT 2,
ADD COLUMN display_condition SMALLINT NOT NULL DEFAULT 1;

UPDATE rb_hint
SET title_display_condition = CASE WHEN title_hidden THEN 2 ELSE 0 END;

ALTER TABLE rb_hint
ADD CONSTRAINT rb_ck_hint_title_display_condition
    CHECK (title_display_condition BETWEEN 0 AND 3),
ADD CONSTRAINT rb_ck_hint_display_condition
    CHECK (display_condition BETWEEN 0 AND 3),
DROP COLUMN title_hidden;

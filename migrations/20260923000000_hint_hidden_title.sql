ALTER TABLE rb_hint
ADD COLUMN hidden_title VARCHAR(120),
ADD CONSTRAINT rb_ck_hint_hidden_title
CHECK (
    hidden_title IS NULL OR (
        CHAR_LENGTH(BTRIM(hidden_title)) BETWEEN 1 AND 120
    )
);

CREATE TABLE rb_user (
    id              SERIAL PRIMARY KEY,
    email           VARCHAR(60) UNIQUE NOT NULL,
    upass           VARCHAR(72) NOT NULL,
    urole           SMALLINT NOT NULL DEFAULT 1,
    nickname        VARCHAR(60) NOT NULL DEFAULT '',
    bio             TEXT,
    ctime_at        TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX rb_idx_user_email ON rb_user(email);

CREATE OR REPLACE FUNCTION rb_user_def_nickname()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.nickname IS NULL OR NEW.nickname = '' THEN
        NEW.nickname := 'user_' || NEW.id::text;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER rb_trg_user_def_nickname
BEFORE INSERT ON rb_user
FOR EACH ROW
EXECUTE FUNCTION rb_user_def_nickname();

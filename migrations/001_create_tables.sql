-- user

CREATE TABLE rb_user (
    id              SERIAL PRIMARY KEY,
    email           VARCHAR(60) UNIQUE NOT NULL,
    pass            VARCHAR(72) NOT NULL,
    urole           SMALLINT NOT NULL DEFAULT 1,
    nickname        VARCHAR(60) NOT NULL DEFAULT '',
    bio             TEXT,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
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

-- team

CREATE TABLE rb_team (
    id              SERIAL PRIMARY KEY,
    tname           VARCHAR(60) NOT NULL,
    pass            VARCHAR(32) NOT NULL,
    bio             TEXT NOT NULL,
    locked          BOOLEAN NOT NULL DEFAULT FALSE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- team member

CREATE TABLE rb_team_member (
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    user_id         INT NOT NULL REFERENCES rb_user(id) ON DELETE CASCADE,
    is_captain      BOOLEAN NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, user_id)
);

CREATE INDEX rb_idx_team_member_team ON rb_team_member(team_id);
CREATE INDEX rb_idx_team_member_user ON rb_team_member(user_id);

-- game

CREATE TABLE rb_game (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(60) NOT NULL,
    shown           BOOLEAN NOT NULL DEFAULT FALSE,
    reg_open_at     TIMESTAMPTZ,
    pre_open_at     TIMESTAMPTZ,
    start_at        TIMESTAMPTZ NOT NULL,
    end_at          TIMESTAMPTZ NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- game entry (participation)

CREATE TABLE rb_game_entry (
    game_id         INT REFERENCES rb_game(id),
    team_id         INT REFERENCES rb_team(id),
    estate          SMALLINT NOT NULL DEFAULT 0,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (game_id, team_id)
);

CREATE INDEX rb_idx_game_entry_game ON rb_game_entry(game_id);
CREATE INDEX rb_idx_game_entry_team ON rb_game_entry(team_id);

-- announcement

CREATE TABLE rb_anmt (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    pinned          BOOLEAN NOT NULL DEFAULT FALSE,
    shown           BOOLEAN NOT NULL DEFAULT FALSE,
    game_id         INT REFERENCES rb_game(id),
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- area

CREATE TABLE rb_area (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    atype           SMALLINT NOT NULL DEFAULT 1
);

-- puzzle

CREATE TABLE rb_puzzle (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    ptype           INT NOT NULL,
    judge           TEXT NOT NULL,
    area_id         INT REFERENCES rb_area(id),
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

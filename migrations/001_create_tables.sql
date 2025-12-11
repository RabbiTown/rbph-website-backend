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

-- game

CREATE TABLE rb_game (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(60) NOT NULL,
    cover           TEXT,
    is_shown        BOOLEAN NOT NULL DEFAULT FALSE,
    is_online       BOOLEAN NOT NULL DEFAULT FALSE,
    reg_open_at     TIMESTAMPTZ,
    pre_open_at     TIMESTAMPTZ,
    start_at        TIMESTAMPTZ NOT NULL,
    end_at          TIMESTAMPTZ NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- team

CREATE TABLE rb_team (
    id              SERIAL PRIMARY KEY,
    tname           VARCHAR(60) NOT NULL,
    tstate          SMALLINT NOT NULL DEFAULT 0,
    pass            VARCHAR(32) NOT NULL,
    bio             TEXT NOT NULL,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- team member

CREATE TABLE rb_team_member (
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    user_id         INT NOT NULL REFERENCES rb_user(id) ON DELETE CASCADE,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    is_captain      BOOLEAN NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, user_id),
    UNIQUE (user_id, game_id)
);

CREATE INDEX rb_idx_team_member_team ON rb_team_member(team_id);
CREATE INDEX rb_idx_team_member_user ON rb_team_member(user_id);

CREATE UNIQUE INDEX rb_idx_team_captain_unique
ON rb_team_member (team_id)
WHERE is_captain = TRUE;

CREATE OR REPLACE FUNCTION rb_team_member_set_game_id()
RETURNS TRIGGER AS $$
BEGIN
    SELECT game_id INTO NEW.game_id FROM rb_team WHERE id = NEW.team_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER rb_trg_team_member_set_game_id
BEFORE INSERT ON rb_team_member
FOR EACH ROW
EXECUTE FUNCTION rb_team_member_set_game_id();

-- announcement

CREATE TABLE rb_anmt (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    is_pinned       BOOLEAN NOT NULL DEFAULT FALSE,
    is_shown        BOOLEAN NOT NULL DEFAULT FALSE,
    game_id         INT REFERENCES rb_game(id),
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- round

CREATE TABLE rb_round (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    cover           TEXT,
    game_id         INT NOT NULL REFERENCES rb_game(id),
    puzzle          INT,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- puzzle

CREATE TABLE rb_puzzle (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    ptype           SMALLINT NOT NULL DEFAULT 0,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    judge           TEXT NOT NULL,
    unlock_cond     TEXT NOT NULL,
    round_id        INT NOT NULL REFERENCES rb_round(id),
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE rb_round ADD CONSTRAINT rb_fk_round_puzzle
FOREIGN KEY (puzzle) REFERENCES rb_puzzle(id) ON DELETE SET NULL;

-- puzzle unlock

CREATE TABLE rb_team_puzzle(
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    pstate          SMALLINT NOT NULL DEFAULT 0,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- submission

CREATE TABLE rb_submission(
    id              SERIAL PRIMARY KEY,
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    user_id         INT NOT NULL REFERENCES rb_user(id) ON DELETE CASCADE,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    user_answer     TEXT NOT NULL,
    norm_answer     TEXT NOT NULL,
    saction         SMALLINT NOT NULL DEFAULT -1,
    sresult         TEXT,
    real_answer     TEXT,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (team_id, puzzle_id, norm_answer)
);

-- hint

-- CREATE TABLE rb_hint (
--     id              SERIAL PRIMARY KEY,
--     title           VARCHAR(120) NOT NULL,
--     content         TEXT NOT NULL,
--     -- cost
--     puzzle_id       INT REFERENCES rb_puzzle(id),
--     ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
-- );

-- currency

-- CREATE TABLE rb_currency (
--     team_id         SERIAL PRIMARY KEY,
--     ctype           INT NOT NULL,
--     count           INT NOT NULL,
--     growth_rate     INT NOT NULL,
--     utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
-- );

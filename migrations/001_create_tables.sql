-- user

CREATE TABLE rb_user (
    id              SERIAL PRIMARY KEY,
    email           VARCHAR(255) UNIQUE NOT NULL,
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
    settings        JSONB NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE rb_game ADD CONSTRAINT rb_ck_game_settings_object
CHECK (jsonb_typeof(settings) = 'object');

-- release phase

CREATE TABLE rb_release_phase (
    id              SERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    title           VARCHAR(120) NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    content_type    SMALLINT NOT NULL DEFAULT 0,
    release_at      TIMESTAMPTZ NOT NULL,
    visibility      SMALLINT NOT NULL DEFAULT 0,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (id, game_id),
    UNIQUE (game_id, release_at)
);

ALTER TABLE rb_release_phase ADD CONSTRAINT rb_ck_release_phase_visibility
CHECK (visibility IN (0, 1));

CREATE INDEX rb_idx_release_phase_due
ON rb_release_phase(release_at, id);

CREATE TABLE rb_release_phase_feature_change (
    phase_id        INT NOT NULL,
    game_id         INT NOT NULL,
    feature_type    SMALLINT NOT NULL,
    target_state    SMALLINT NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (phase_id, feature_type),
    FOREIGN KEY (phase_id, game_id)
        REFERENCES rb_release_phase(id, game_id) ON DELETE CASCADE
);

ALTER TABLE rb_release_phase_feature_change ADD CONSTRAINT rb_ck_release_phase_feature_change
CHECK (
    (feature_type = 0 AND target_state IN (0, 1)) OR
    (feature_type IN (1, 2) AND target_state IN (0, 1, 2)) OR
    (feature_type = 3 AND target_state IN (0, 1))
);

CREATE TABLE rb_game_feature (
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    feature_type    SMALLINT NOT NULL,
    state           SMALLINT NOT NULL,
    source_phase_id INT REFERENCES rb_release_phase(id) ON DELETE SET NULL,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (game_id, feature_type)
);

ALTER TABLE rb_game_feature ADD CONSTRAINT rb_ck_game_feature_state
CHECK (
    (feature_type = 0 AND state IN (0, 1)) OR
    (feature_type IN (1, 2) AND state IN (0, 1, 2)) OR
    (feature_type = 3 AND state IN (0, 1))
);

CREATE TABLE rb_game_feature_history (
    id              BIGSERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    feature_type    SMALLINT NOT NULL,
    old_state       SMALLINT NOT NULL,
    new_state       SMALLINT NOT NULL,
    phase_id        INT REFERENCES rb_release_phase(id) ON DELETE SET NULL,
    actor_id        INT REFERENCES rb_user(id) ON DELETE SET NULL,
    effective_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX rb_idx_game_feature_history_phase
ON rb_game_feature_history(phase_id, feature_type)
WHERE phase_id IS NOT NULL;

CREATE TABLE rb_release_event (
    id              BIGSERIAL PRIMARY KEY,
    phase_id        INT NOT NULL UNIQUE REFERENCES rb_release_phase(id) ON DELETE CASCADE,
    occurred_at     TIMESTAMPTZ NOT NULL,
    notified_at     TIMESTAMPTZ,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX rb_idx_release_event_pending_notification
ON rb_release_event(id)
WHERE notified_at IS NULL;

-- team

CREATE TABLE rb_team (
    id              SERIAL PRIMARY KEY,
    name           VARCHAR(60) NOT NULL,
    state           SMALLINT NOT NULL DEFAULT 0,
    pass            VARCHAR(32) NOT NULL,
    bio             TEXT NOT NULL,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finish_at       TIMESTAMPTZ
);

CREATE INDEX rb_idx_team_game_state_finish
ON rb_team(game_id, state, finish_at);

CREATE TABLE rb_leaderboard_lock (
    game_id         INT PRIMARY KEY REFERENCES rb_game(id) ON DELETE CASCADE,
    phase_id        INT REFERENCES rb_release_phase(id) ON DELETE SET NULL,
    locked_at       TIMESTAMPTZ NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE rb_leaderboard_lock_team (
    game_id         INT NOT NULL REFERENCES rb_leaderboard_lock(game_id) ON DELETE CASCADE,
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    rank            INT NOT NULL,
    solves          BIGINT NOT NULL,
    finish_at       TIMESTAMPTZ,
    last_solved_at  TIMESTAMPTZ,
    PRIMARY KEY (game_id, team_id),
    UNIQUE (game_id, rank)
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

CREATE INDEX rb_idx_team_member_team_captain
ON rb_team_member(team_id, is_captain, user_id);

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

-- round

CREATE TABLE rb_round (
    id              SERIAL PRIMARY KEY,
    slug            VARCHAR(120),
    sort            INT NOT NULL DEFAULT 0,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    cover           TEXT,
    game_id         INT NOT NULL REFERENCES rb_game(id),
    puzzle          INT,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX rb_idx_round_game_slug
ON rb_round(game_id, slug)
WHERE slug IS NOT NULL;

ALTER TABLE rb_round ADD CONSTRAINT rb_ck_round_slug_atom
CHECK (slug IS NULL OR slug ~ '^[A-Za-z_][A-Za-z0-9_-]*$');

-- puzzle

CREATE TABLE rb_puzzle (
    id              SERIAL PRIMARY KEY,
    slug            VARCHAR(120),
    sort            INT NOT NULL DEFAULT 0,
    title           VARCHAR(120) NOT NULL,
    ptype           SMALLINT NOT NULL DEFAULT 0,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    judge           JSONB NOT NULL DEFAULT '{}',
    penalty         JSONB NOT NULL DEFAULT '[]',
    max_submit      INT,
    unlock_cond     TEXT NOT NULL,
    release_phase_id INT,
    round_id        INT NOT NULL REFERENCES rb_round(id),
    game_id         INT NOT NULL REFERENCES rb_game(id),
    ticket_enabled  BOOLEAN NOT NULL DEFAULT TRUE,
    ticket_cooldown INT NOT NULL DEFAULT 0,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE OR REPLACE FUNCTION rb_puzzle_set_game_id()
RETURNS TRIGGER AS $$
BEGIN
    SELECT game_id INTO NEW.game_id FROM rb_round WHERE id = NEW.round_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER rb_trg_puzzle_set_game_id
BEFORE INSERT OR UPDATE OF round_id ON rb_puzzle
FOR EACH ROW
EXECUTE FUNCTION rb_puzzle_set_game_id();

ALTER TABLE rb_puzzle ADD CONSTRAINT rb_fk_puzzle_release_phase_game
FOREIGN KEY (release_phase_id, game_id)
REFERENCES rb_release_phase(id, game_id);

CREATE UNIQUE INDEX rb_idx_puzzle_game_slug
ON rb_puzzle(game_id, slug)
WHERE slug IS NOT NULL;

ALTER TABLE rb_puzzle ADD CONSTRAINT rb_ck_puzzle_slug_atom
CHECK (slug IS NULL OR slug ~ '^[A-Za-z_][A-Za-z0-9_-]*$');

-- asset group

CREATE TABLE rb_asset_group (
    id              SERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    round_id        INT REFERENCES rb_round(id) ON DELETE CASCADE,
    backend         VARCHAR(32) NOT NULL,
    object_key      TEXT NOT NULL UNIQUE,
    original_name   VARCHAR(255) NOT NULL,
    mime_type       VARCHAR(120) NOT NULL,
    size            BIGINT NOT NULL,
    sha256          CHAR(64) NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE rb_asset_group ADD CONSTRAINT rb_ck_asset_group_scope
CHECK ((puzzle_id IS NOT NULL)::INT + (round_id IS NOT NULL)::INT <= 1);

CREATE INDEX rb_idx_asset_group_game_puzzle_ctime
ON rb_asset_group(game_id, puzzle_id, ctime_at DESC);

CREATE INDEX rb_idx_asset_group_game_round_ctime
ON rb_asset_group(game_id, round_id, ctime_at DESC);

CREATE TABLE rb_asset_file (
    id              SERIAL PRIMARY KEY,
    group_id        INT NOT NULL REFERENCES rb_asset_group(id) ON DELETE CASCADE,
    relative_path   TEXT NOT NULL,
    mime_type       VARCHAR(120) NOT NULL,
    size            BIGINT NOT NULL,
    sha256          CHAR(64) NOT NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (group_id, relative_path)
);

CREATE INDEX rb_idx_asset_file_group_path
ON rb_asset_file(group_id, relative_path);

ALTER TABLE rb_round ADD CONSTRAINT rb_fk_round_puzzle
FOREIGN KEY (puzzle) REFERENCES rb_puzzle(id) ON DELETE SET NULL;

CREATE TABLE rb_team_puzzle(
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    state           SMALLINT NOT NULL DEFAULT 0,
    max_submit      INT NOT NULL DEFAULT 0,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    solve_at        TIMESTAMPTZ,
    cooldown_till   TIMESTAMPTZ,
    PRIMARY KEY (team_id, puzzle_id)
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
    ignored         BOOLEAN NOT NULL DEFAULT FALSE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX rb_idx_submission_team_puzzle_norm
ON rb_submission(team_id, puzzle_id, norm_answer)
WHERE ignored = FALSE;

-- currency

CREATE TABLE rb_currency (
    id              SERIAL PRIMARY KEY,
    cname           VARCHAR(40) NOT NULL,
    slug            VARCHAR(40) NOT NULL,
    growth          BIGINT NOT NULL,
    init_amount     BIGINT NOT NULL DEFAULT 0,
    init_hidden     BOOLEAN NOT NULL DEFAULT FALSE,
    max_amount      BIGINT NOT NULL DEFAULT 9223372036854775807,
    prec            INT NOT NULL,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    UNIQUE (game_id, slug)
);

CREATE TABLE rb_team_currency (
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    currency_id     INT NOT NULL REFERENCES rb_currency(id) ON DELETE CASCADE,
    amount          BIGINT NOT NULL DEFAULT 0,
    growth          BIGINT NOT NULL DEFAULT 0,
    hidden          BOOLEAN NOT NULL DEFAULT FALSE,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, currency_id)
);

-- hint

CREATE TABLE rb_hint (
    id              SERIAL PRIMARY KEY,
    sort            INT NOT NULL DEFAULT 0,
    title           VARCHAR(120) NOT NULL,
    title_hidden    BOOLEAN NOT NULL DEFAULT TRUE,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    cooldown        INT NOT NULL DEFAULT 0,
    cost_id         INT REFERENCES rb_currency(id) ON DELETE SET NULL,
    cost_amount     BIGINT NOT NULL DEFAULT 0,
    backend_function VARCHAR(64),
    puzzle_id       INT NOT NULL REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE rb_team_hint (
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    hint_id         INT NOT NULL REFERENCES rb_hint(id) ON DELETE CASCADE,
    unlocked        BOOLEAN NOT NULL DEFAULT TRUE,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (team_id, hint_id)
);

-- announcement

CREATE TABLE rb_announcement (
    id              SERIAL PRIMARY KEY,
    title           VARCHAR(120) NOT NULL,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 0,
    is_pinned       BOOLEAN NOT NULL DEFAULT FALSE,
    is_shown        BOOLEAN NOT NULL DEFAULT FALSE,
    game_id         INT REFERENCES rb_game(id) ON DELETE CASCADE,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- ticket

CREATE TABLE rb_ticket (
    id              SERIAL PRIMARY KEY,
    state           SMALLINT NOT NULL,
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE CASCADE,
    assignee        INT REFERENCES rb_user(id) ON DELETE SET NULL
);

CREATE UNIQUE INDEX rb_idx_ticket_dm_unique
ON rb_ticket(team_id)
WHERE puzzle_id IS NULL;

CREATE INDEX rb_idx_ticket_open_team_puzzle
ON rb_ticket(state, team_id, puzzle_id);

CREATE INDEX rb_idx_ticket_assignee_state
ON rb_ticket(assignee, state);

CREATE TABLE rb_message (
    id              SERIAL PRIMARY KEY,
    content         TEXT NOT NULL,
    content_type    SMALLINT NOT NULL DEFAULT 2,
    sender          INT NOT NULL REFERENCES rb_user(id) ON DELETE CASCADE,
    sender_type     SMALLINT NOT NULL,
    cost_id         INT REFERENCES rb_currency(id) ON DELETE SET NULL,
    cost_amount     BIGINT NOT NULL DEFAULT 0,
    unlocked        BOOLEAN NOT NULL DEFAULT TRUE,
    ticket_id       INT NOT NULL REFERENCES rb_ticket(id) ON DELETE CASCADE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    utime_at        TIMESTAMPTZ
);

CREATE INDEX rb_idx_message_ticket_host_id_partial
ON rb_message(sender_type, ticket_id, id);

CREATE INDEX rb_idx_message_ticket_id
ON rb_message(ticket_id, id DESC);

-- notification

CREATE TABLE rb_notification (
    id              BIGSERIAL PRIMARY KEY,
    team_id         INT NOT NULL REFERENCES rb_team(id) ON DELETE CASCADE,
    kind            SMALLINT NOT NULL,
    source_id       INT NOT NULL,
    actor           INT REFERENCES rb_user(id) ON DELETE SET NULL,
    data            JSONB NOT NULL,
    read_at         TIMESTAMPTZ,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (kind, source_id)
);

ALTER TABLE rb_notification ADD CONSTRAINT rb_ck_notification_data_object
CHECK (jsonb_typeof(data) = 'object');

CREATE INDEX rb_idx_notification_team_unread
ON rb_notification(team_id, id DESC)
WHERE read_at IS NULL;

CREATE TABLE rb_ticket_operation (
    id              SERIAL PRIMARY KEY,
    ticket_id       INT NOT NULL REFERENCES rb_ticket(id) ON DELETE CASCADE,
    action          SMALLINT NOT NULL,
    actor           INT NOT NULL REFERENCES rb_user(id) ON DELETE CASCADE,
    actor_type      SMALLINT NOT NULL,
    message_id      INT REFERENCES rb_message(id) ON DELETE SET NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX rb_idx_ticket_operation_ticket_id
ON rb_ticket_operation(ticket_id, ctime_at DESC, id DESC);

-- event log

CREATE TABLE rb_event_log (
    id              BIGSERIAL PRIMARY KEY,
    event_type      VARCHAR(96) NOT NULL,
    event_scope     SMALLINT NOT NULL,
    severity        SMALLINT NOT NULL DEFAULT 0,
    game_id         INT REFERENCES rb_game(id) ON DELETE CASCADE,
    team_id         INT REFERENCES rb_team(id) ON DELETE SET NULL,
    user_id         INT REFERENCES rb_user(id) ON DELETE SET NULL,
    target_user_id  INT REFERENCES rb_user(id) ON DELETE SET NULL,
    puzzle_id       INT REFERENCES rb_puzzle(id) ON DELETE SET NULL,
    round_id        INT REFERENCES rb_round(id) ON DELETE SET NULL,
    hint_id         INT REFERENCES rb_hint(id) ON DELETE SET NULL,
    ticket_id       INT REFERENCES rb_ticket(id) ON DELETE SET NULL,
    submission_id   INT REFERENCES rb_submission(id) ON DELETE SET NULL,
    currency_id     INT REFERENCES rb_currency(id) ON DELETE SET NULL,
    delta_amount    BIGINT,
    data            JSONB NOT NULL DEFAULT '{}'::JSONB,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX rb_idx_event_log_team_ctime
ON rb_event_log(team_id, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_event_log_game_scope_ctime
ON rb_event_log(game_id, event_scope, ctime_at DESC, id DESC);

CREATE INDEX rb_idx_event_log_puzzle_ctime
ON rb_event_log(puzzle_id, ctime_at DESC, id DESC)
WHERE puzzle_id IS NOT NULL;

CREATE INDEX rb_idx_event_log_currency_ctime
ON rb_event_log(currency_id, ctime_at DESC, id DESC)
WHERE currency_id IS NOT NULL;

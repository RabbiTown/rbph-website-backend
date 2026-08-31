CREATE TABLE rb_frontend_package (
    id              SERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    asset_group_id  INT NOT NULL UNIQUE REFERENCES rb_asset_group(id) ON DELETE RESTRICT,
    name            VARCHAR(120) NOT NULL,
    version         VARCHAR(60) NOT NULL,
    manifest_path   TEXT NOT NULL DEFAULT 'rbph-theme.json',
    manifest        JSONB NOT NULL,
    sha256          CHAR(64) NOT NULL,
    delete_pending  BOOLEAN NOT NULL DEFAULT FALSE,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX rb_idx_frontend_package_active_name
ON rb_frontend_package(game_id, name) WHERE NOT delete_pending;

CREATE TABLE rb_frontend_revision (
    id              BIGSERIAL PRIMARY KEY,
    game_id         INT NOT NULL REFERENCES rb_game(id) ON DELETE CASCADE,
    revision        BIGINT NOT NULL,
    status          VARCHAR(16) NOT NULL,
    created_by      INT REFERENCES rb_user(id) ON DELETE SET NULL,
    ctime_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    published_at    TIMESTAMPTZ,
    UNIQUE (game_id, revision),
    CHECK (status IN ('draft', 'published'))
);

CREATE UNIQUE INDEX rb_idx_frontend_revision_draft
ON rb_frontend_revision(game_id) WHERE status = 'draft';

CREATE UNIQUE INDEX rb_idx_frontend_revision_published
ON rb_frontend_revision(game_id) WHERE status = 'published';

CREATE TABLE rb_frontend_binding (
    revision_id     BIGINT NOT NULL REFERENCES rb_frontend_revision(id) ON DELETE CASCADE,
    surface         VARCHAR(32) NOT NULL,
    scope_kind      VARCHAR(16) NOT NULL,
    scope_id        INT NOT NULL DEFAULT 0,
    package_id      INT REFERENCES rb_frontend_package(id) ON DELETE RESTRICT,
    renderer_id     VARCHAR(120),
    PRIMARY KEY (revision_id, surface, scope_kind, scope_id),
    CHECK (surface IN ('round-page', 'puzzle-page')),
    CHECK (scope_kind IN ('game', 'round', 'puzzle')),
    CHECK ((scope_kind = 'game' AND scope_id = 0) OR (scope_kind <> 'game' AND scope_id > 0)),
    CHECK (surface <> 'round-page' OR scope_kind <> 'puzzle'),
    CONSTRAINT rb_frontend_binding_renderer_pair CHECK (
        (package_id IS NULL AND renderer_id IS NULL)
        OR (package_id IS NOT NULL AND renderer_id IS NOT NULL)
    )
);

CREATE TABLE rb_frontend_feature_activation (
    revision_id     BIGINT NOT NULL REFERENCES rb_frontend_revision(id) ON DELETE CASCADE,
    package_id      INT NOT NULL REFERENCES rb_frontend_package(id) ON DELETE RESTRICT,
    feature         SMALLINT NOT NULL CHECK (feature BETWEEN 0 AND 2),
    PRIMARY KEY (revision_id, package_id, feature)
);

CREATE INDEX rb_idx_frontend_feature_activation_package
ON rb_frontend_feature_activation(package_id);

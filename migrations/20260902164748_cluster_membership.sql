CREATE TABLE rb_cluster_config (
    singleton       BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    deployment_mode TEXT NOT NULL CHECK (deployment_mode IN ('single', 'cluster')),
    fingerprint     CHAR(64) NOT NULL,
    generation      BIGINT NOT NULL CHECK (generation > 0),
    established_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE rb_cluster_instance (
    instance_id     UUID PRIMARY KEY,
    deployment_mode TEXT NOT NULL CHECK (deployment_mode IN ('single', 'cluster')),
    fingerprint     CHAR(64) NOT NULL,
    generation      BIGINT NOT NULL CHECK (generation > 0),
    started_at      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_until     TIMESTAMPTZ NOT NULL
);

CREATE INDEX rb_idx_cluster_instance_lease
ON rb_cluster_instance(lease_until);

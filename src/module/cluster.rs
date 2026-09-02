use std::{
    sync::Mutex,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use sqlx::{Connection, PgConnection, Row, migrate::Migrator};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{DbPool, config::DeploymentMode, error::RbInternalError};

const DEPLOYMENT_GATE_LOCK_KEY: i64 = 0x0052_4250_4843_4C55;
const LEASE_SECONDS: i32 = 60;
const RENEW_INTERVAL: Duration = Duration::from_secs(15);
const RENEW_RETRY: Duration = Duration::from_secs(3);
const RENEW_TIMEOUT: Duration = Duration::from_secs(3);
const LEASE_SAFETY_WINDOW: Duration = Duration::from_secs(30);
pub const HTTP_SHUTDOWN_TIMEOUT_SECONDS: u64 = 10;

pub struct ClusterMembership {
    pool: DbPool,
    instance_id: Uuid,
    mode: DeploymentMode,
    fingerprint: String,
    generation: i64,
    ready: AtomicBool,
    terminal: AtomicBool,
    stopping: AtomicBool,
    last_renewed: Mutex<Instant>,
    lost: Notify,
    stop: Notify,
}

impl ClusterMembership {
    pub async fn register(
        pool: DbPool,
        mode: DeploymentMode,
        fingerprint: String,
    ) -> Result<Arc<Self>, RbInternalError> {
        let mut conn = pool.acquire().await?;
        acquire_deployment_gate(&mut conn).await?;
        let result = Self::register_on_connection(pool, &mut conn, mode, fingerprint).await;
        let unlock_result = release_deployment_gate(&mut conn).await;
        merge_unlock_result(result, unlock_result)
    }

    pub async fn migrate_and_register(
        pool: DbPool,
        migrator: &Migrator,
        mode: DeploymentMode,
        fingerprint: String,
    ) -> Result<Arc<Self>, RbInternalError> {
        let mut conn = pool.acquire().await?;
        acquire_deployment_gate(&mut conn).await?;
        let result = async {
            let expected = migrator
                .iter()
                .map(|migration| migration.version)
                .max()
                .ok_or("at least one embedded migration is required")?;
            match inspect_schema_state(&mut conn, expected).await? {
                SchemaState::Current => {}
                SchemaState::Behind => ensure_no_active_instances(&mut conn).await?,
                SchemaState::Ahead(version) => {
                    return Err(format!(
                        "database schema version {version} is newer than this build ({expected})"
                    )
                    .into());
                }
            }

            migrator.run_direct(&mut *conn).await.map_err(|error| {
                RbInternalError::Other(format!("database migration failed: {error}"))
            })?;

            Self::register_on_connection(pool, &mut conn, mode, fingerprint).await
        }
        .await;
        let unlock_result = release_deployment_gate(&mut conn).await;
        merge_unlock_result(result, unlock_result)
    }

    async fn register_on_connection(
        pool: DbPool,
        conn: &mut PgConnection,
        mode: DeploymentMode,
        fingerprint: String,
    ) -> Result<Arc<Self>, RbInternalError> {
        let instance_id = Uuid::new_v4();
        let generation = register_instance(conn, instance_id, mode, &fingerprint).await?;
        let membership = Arc::new(Self {
            pool,
            instance_id,
            mode,
            fingerprint,
            generation,
            ready: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            last_renewed: Mutex::new(Instant::now()),
            lost: Notify::new(),
            stop: Notify::new(),
        });
        log::info!(
            "registered instance {} (mode={}, generation={}, fingerprint={})",
            membership.instance_id,
            membership.mode.as_str(),
            membership.generation,
            membership.fingerprint_prefix(),
        );
        Ok(membership)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
            && !self.terminal.load(Ordering::Acquire)
            && self
                .last_renewed
                .lock()
                .is_ok_and(|renewed| renewed.elapsed() < LEASE_SAFETY_WINDOW)
    }

    pub async fn wait_lost(&self) {
        while !self.terminal.load(Ordering::Acquire) {
            self.lost.notified().await;
        }
    }

    pub async fn run(self: Arc<Self>) {
        let mut last_success = Instant::now();
        let mut delay = Duration::ZERO;
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.stop.notified() => return,
            }
            if self.stopping.load(Ordering::Acquire) {
                return;
            }

            match tokio::time::timeout(RENEW_TIMEOUT, self.renew()).await {
                Ok(Ok(true)) => {
                    last_success = Instant::now();
                    if let Ok(mut renewed) = self.last_renewed.lock() {
                        *renewed = last_success;
                    }
                    self.ready.store(true, Ordering::Release);
                    delay = RENEW_INTERVAL;
                }
                Ok(Ok(false)) => {
                    self.mark_lost("membership lease no longer exists");
                    return;
                }
                Ok(Err(error)) => {
                    log::warn!(
                        "failed to renew cluster membership for instance {}: {error}",
                        self.instance_id
                    );
                    if last_success.elapsed() >= LEASE_SAFETY_WINDOW {
                        self.mark_lost("membership lease could not be renewed safely");
                        return;
                    }
                    delay = RENEW_RETRY;
                }
                Err(_) => {
                    log::warn!(
                        "cluster membership renewal timed out for instance {}",
                        self.instance_id
                    );
                    if last_success.elapsed() >= LEASE_SAFETY_WINDOW {
                        self.mark_lost("membership lease renewal timed out");
                        return;
                    }
                    delay = RENEW_RETRY;
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.ready.store(false, Ordering::Release);
        self.terminal.store(true, Ordering::Release);
        self.stop.notify_one();
        self.lost.notify_one();
        if let Err(error) =
            sqlx::query("DELETE FROM rb_cluster_instance WHERE instance_id = $1::UUID")
                .bind(self.instance_id.to_string())
                .execute(&self.pool)
                .await
        {
            log::warn!(
                "failed to unregister cluster instance {}: {error}",
                self.instance_id
            );
        }
    }

    async fn renew(&self) -> Result<bool, RbInternalError> {
        let result = sqlx::query(
            "UPDATE rb_cluster_instance
             SET lease_until = clock_timestamp() + make_interval(secs => $5)
             WHERE instance_id = $1::UUID AND deployment_mode = $2 AND fingerprint = $3
               AND generation = $4 AND lease_until > clock_timestamp()",
        )
        .bind(self.instance_id.to_string())
        .bind(self.mode.as_str())
        .bind(&self.fingerprint)
        .bind(self.generation)
        .bind(LEASE_SECONDS)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    fn mark_lost(&self, reason: &str) {
        self.ready.store(false, Ordering::Release);
        if !self.terminal.swap(true, Ordering::AcqRel) {
            log::error!(
                "cluster membership lost for instance {}: {reason}",
                self.instance_id
            );
            self.lost.notify_one();
        }
    }

    fn fingerprint_prefix(&self) -> &str {
        self.fingerprint.get(..12).unwrap_or(&self.fingerprint)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum SchemaState {
    Behind,
    Current,
    Ahead(i64),
}

async fn acquire_deployment_gate(conn: &mut PgConnection) -> Result<(), RbInternalError> {
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(DEPLOYMENT_GATE_LOCK_KEY)
        .execute(conn)
        .await?;
    Ok(())
}

async fn release_deployment_gate(conn: &mut PgConnection) -> Result<(), RbInternalError> {
    let unlocked = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
        .bind(DEPLOYMENT_GATE_LOCK_KEY)
        .fetch_one(conn)
        .await?;
    if !unlocked {
        return Err("deployment gate was not held by this connection".into());
    }
    Ok(())
}

fn merge_unlock_result<T>(
    result: Result<T, RbInternalError>,
    unlock: Result<(), RbInternalError>,
) -> Result<T, RbInternalError> {
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

async fn inspect_schema_state(
    conn: &mut PgConnection,
    expected: i64,
) -> Result<SchemaState, RbInternalError> {
    let migrations_table_exists =
        sqlx::query_scalar::<_, bool>("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *conn)
            .await?;
    if !migrations_table_exists {
        return Ok(SchemaState::Behind);
    }

    let latest = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(version) FROM _sqlx_migrations WHERE success",
    )
    .fetch_one(conn)
    .await?;
    Ok(match latest {
        Some(version) if version > expected => SchemaState::Ahead(version),
        Some(version) if version == expected => SchemaState::Current,
        _ => SchemaState::Behind,
    })
}

async fn ensure_no_active_instances(conn: &mut PgConnection) -> Result<(), RbInternalError> {
    let membership_table_exists = sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('public.rb_cluster_instance') IS NOT NULL",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !membership_table_exists {
        return Ok(());
    }

    sqlx::query("DELETE FROM rb_cluster_instance WHERE lease_until <= clock_timestamp()")
        .execute(&mut *conn)
        .await?;
    let active = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1 FROM rb_cluster_instance WHERE lease_until > clock_timestamp()
        )",
    )
    .fetch_one(conn)
    .await?;
    if active {
        return Err("database migration requires all active instances to stop first".into());
    }
    Ok(())
}

async fn register_instance(
    conn: &mut PgConnection,
    instance_id: Uuid,
    mode: DeploymentMode,
    fingerprint: &str,
) -> Result<i64, RbInternalError> {
    let mut tx = conn.begin().await?;
    sqlx::query("DELETE FROM rb_cluster_instance WHERE lease_until <= clock_timestamp()")
        .execute(&mut *tx)
        .await?;

    let active = sqlx::query(
        "SELECT deployment_mode, fingerprint, generation
         FROM rb_cluster_instance
         WHERE lease_until > clock_timestamp()
         ORDER BY started_at, instance_id",
    )
    .fetch_all(&mut *tx)
    .await?;

    let generation = if active.is_empty() {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO rb_cluster_config (
                singleton, deployment_mode, fingerprint, generation
             ) VALUES (TRUE, $1, $2, 1)
             ON CONFLICT (singleton) DO UPDATE SET
                generation = CASE
                    WHEN rb_cluster_config.deployment_mode = EXCLUDED.deployment_mode
                     AND rb_cluster_config.fingerprint = EXCLUDED.fingerprint
                    THEN rb_cluster_config.generation
                    ELSE rb_cluster_config.generation + 1
                END,
                deployment_mode = EXCLUDED.deployment_mode,
                fingerprint = EXCLUDED.fingerprint,
                established_at = CASE
                    WHEN rb_cluster_config.deployment_mode = EXCLUDED.deployment_mode
                     AND rb_cluster_config.fingerprint = EXCLUDED.fingerprint
                    THEN rb_cluster_config.established_at
                    ELSE clock_timestamp()
                END,
                updated_at = clock_timestamp()
             RETURNING generation",
        )
        .bind(mode.as_str())
        .bind(fingerprint)
        .fetch_one(&mut *tx)
        .await?
    } else {
        if mode == DeploymentMode::Single {
            return Err("single deployment mode already has an active instance".into());
        }
        for row in &active {
            let active_mode: String = row.try_get("deployment_mode")?;
            let active_fingerprint: String = row.try_get("fingerprint")?;
            if active_mode != mode.as_str() || active_fingerprint != fingerprint {
                return Err(
                    "active instances use a different deployment mode or cluster configuration"
                        .into(),
                );
            }
        }
        let generation: i64 = active[0].try_get("generation")?;
        let baseline_matches = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                SELECT 1 FROM rb_cluster_config
                WHERE singleton = TRUE AND deployment_mode = $1
                  AND fingerprint = $2 AND generation = $3
             )",
        )
        .bind(mode.as_str())
        .bind(fingerprint)
        .bind(generation)
        .fetch_one(&mut *tx)
        .await?;
        if !baseline_matches {
            return Err("cluster configuration baseline is inconsistent".into());
        }
        generation
    };

    sqlx::query(
        "INSERT INTO rb_cluster_instance (
            instance_id, deployment_mode, fingerprint, generation, lease_until
         ) VALUES ($1::UUID, $2, $3, $4, clock_timestamp() + make_interval(secs => $5))",
    )
    .bind(instance_id.to_string())
    .bind(mode.as_str())
    .bind(fingerprint)
    .bind(generation)
    .bind(LEASE_SECONDS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(generation)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::{
        ClusterMembership, DeploymentMode, HTTP_SHUTDOWN_TIMEOUT_SECONDS, LEASE_SAFETY_WINDOW,
        LEASE_SECONDS, RENEW_TIMEOUT, SchemaState, ensure_no_active_instances,
        inspect_schema_state,
    };

    #[test]
    fn shutdown_budget_finishes_well_before_lease_expiry() {
        let remaining_after_loss =
            Duration::from_secs(LEASE_SECONDS as u64) - LEASE_SAFETY_WINDOW - RENEW_TIMEOUT;
        assert!(Duration::from_secs(HTTP_SHUTDOWN_TIMEOUT_SECONDS) < remaining_after_loss);
    }

    #[sqlx::test]
    async fn matching_cluster_instances_can_register(pool: PgPool) {
        let first =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        assert!(!first.is_ready());
        let first_task = tokio::spawn(first.clone().run());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !first.is_ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first renewal should make membership ready");
        let second =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        assert_eq!(first.generation, second.generation);
        first.shutdown().await;
        second.shutdown().await;
        first_task.await.unwrap();
    }

    #[sqlx::test]
    async fn single_and_mismatched_cluster_instances_are_rejected(pool: PgPool) {
        let single =
            ClusterMembership::register(pool.clone(), DeploymentMode::Single, "a".repeat(64))
                .await
                .unwrap();
        assert!(
            ClusterMembership::register(pool.clone(), DeploymentMode::Single, "a".repeat(64))
                .await
                .is_err()
        );
        assert!(
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .is_err()
        );
        single.shutdown().await;

        let cluster =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        assert!(
            ClusterMembership::register(pool, DeploymentMode::Cluster, "b".repeat(64))
                .await
                .is_err()
        );
        cluster.shutdown().await;
    }

    #[sqlx::test]
    async fn expired_members_allow_configuration_generation_change(pool: PgPool) {
        let first =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        sqlx::query("UPDATE rb_cluster_instance SET lease_until = clock_timestamp()")
            .execute(&pool)
            .await
            .unwrap();
        let second =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "b".repeat(64))
                .await
                .unwrap();
        assert_eq!(second.generation, first.generation + 1);
        second.shutdown().await;
    }

    #[sqlx::test]
    async fn expired_lease_cannot_be_resurrected_and_loss_notification_is_sticky(pool: PgPool) {
        let membership =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        sqlx::query(
            "UPDATE rb_cluster_instance
             SET lease_until = clock_timestamp()
             WHERE instance_id = $1::UUID",
        )
        .bind(membership.instance_id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        assert!(!membership.renew().await.unwrap());

        membership.mark_lost("test");
        tokio::time::timeout(Duration::from_millis(100), membership.wait_lost())
            .await
            .unwrap();
        assert!(!membership.is_ready());
        membership.shutdown().await;
    }

    #[sqlx::test]
    async fn schema_inspection_and_migration_guard_use_active_leases(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            inspect_schema_state(&mut conn, crate::embedded_schema_generation())
                .await
                .unwrap(),
            SchemaState::Current
        );

        let membership =
            ClusterMembership::register(pool.clone(), DeploymentMode::Cluster, "a".repeat(64))
                .await
                .unwrap();
        assert!(ensure_no_active_instances(&mut conn).await.is_err());
        membership.shutdown().await;
        ensure_no_active_instances(&mut conn).await.unwrap();
    }
}

use std::{sync::Arc, time::Duration};

use sqlx::PgConnection;
use tokio::sync::Notify;

use crate::{AppState, db, error::RbInternalError};

const RELEASE_PROCESS_LOCK_KEY: i64 = 0x0052_4250_4852_454C;

async fn try_acquire_process_lock(conn: &mut PgConnection) -> Result<bool, RbInternalError> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock($1)")
            .bind(RELEASE_PROCESS_LOCK_KEY)
            .fetch_one(conn)
            .await?,
    )
}

pub async fn process_due_releases(app: &AppState) -> Result<(), RbInternalError> {
    process_due_releases_with_lock(app, false).await
}

pub async fn process_due_releases_wait(app: &AppState) -> Result<(), RbInternalError> {
    process_due_releases_with_lock(app, true).await
}

async fn process_due_releases_with_lock(
    app: &AppState,
    wait_for_lock: bool,
) -> Result<(), RbInternalError> {
    let mut lock_tx = app.db.begin().await?;
    let locked = if wait_for_lock {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(RELEASE_PROCESS_LOCK_KEY)
            .execute(&mut *lock_tx)
            .await?;
        true
    } else {
        try_acquire_process_lock(&mut lock_tx).await?
    };
    if !locked {
        lock_tx.rollback().await?;
        return Ok(());
    }

    let result = process_due_releases_locked(app).await;
    let release_result = lock_tx.rollback().await;
    match (result, release_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
    }
}

async fn process_due_releases_locked(app: &AppState) -> Result<(), RbInternalError> {
    db::release::materialize_due(&app.db).await?;
    for event in db::release::pending_notifications(&app.db).await? {
        if let Some(phase_id) = event.phase_id
            && db::feature::apply_phase_changes(&app.db, event.game_id, phase_id, event.occurred_at)
                .await?
        {
            db::board::LEADER_BOARD_CACHE
                .invalidate_game(&app.db, event.game_id)
                .await?;
        }
        db::release::mark_content_blocks_dirty(&app.db, event.id, event.phase_id).await?;
        for team_id in db::release::released_team_ids(&app.db, event.id, event.phase_id).await? {
            db::puzzle::refresh_team_hint_enablements(&app.db, team_id, None).await?;
        }
        app.sync_hub
            .notify_game_release_updated(event.game_id, event.id, false)
            .await;
        db::release::mark_notified(&app.db, event.id).await?;
    }
    Ok(())
}

pub fn wake_scheduler(app: &AppState) {
    app.release_schedule_changed.notify_one();
}

pub async fn run_scheduler(app: AppState, changed: Arc<Notify>) {
    loop {
        if let Err(error) = process_due_releases(&app).await {
            log::error!("failed to process due releases: {error}");
        }

        let delay = match db::release::next_delay_seconds(&app.db).await {
            Ok(Some(seconds)) => Duration::from_secs(seconds.min(60)),
            Ok(None) => Duration::from_secs(60),
            Err(error) => {
                log::error!("failed to query next release: {error}");
                Duration::from_secs(15)
            }
        };

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = changed.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::try_acquire_process_lock;

    #[sqlx::test]
    async fn process_lock_is_exclusive_and_released_with_transaction(pool: PgPool) {
        let mut first = pool.begin().await.expect("first transaction should begin");
        let mut second = pool.begin().await.expect("second transaction should begin");

        assert!(
            try_acquire_process_lock(&mut first)
                .await
                .expect("first lock attempt should succeed")
        );
        assert!(
            !try_acquire_process_lock(&mut second)
                .await
                .expect("concurrent lock attempt should complete")
        );

        first
            .rollback()
            .await
            .expect("rolling back should release the first lock");
        assert!(
            try_acquire_process_lock(&mut second)
                .await
                .expect("lock should be available after rollback")
        );
    }
}

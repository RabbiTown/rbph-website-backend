use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;

use crate::{AppState, db, error::RbInternalError};

pub async fn process_due_releases(app: &AppState) -> Result<(), RbInternalError> {
    db::release::materialize_due(&app.db).await?;
    for event in db::release::pending_notifications(&app.db).await? {
        if let Some(phase_id) = event.phase_id
            && db::feature::apply_phase_changes(&app.db, event.game_id, phase_id, event.occurred_at)
                .await?
        {
            db::board::LEADER_BOARD_CACHE
                .invalidate_game(event.game_id)
                .await;
        }
        let (puzzles, rounds) =
            db::release::event_cache_targets(&app.db, event.id, event.phase_id).await?;
        db::release::mark_content_blocks_dirty(&app.db, event.id, event.phase_id).await?;
        for team_id in db::release::released_team_ids(&app.db, event.id, event.phase_id).await? {
            db::puzzle::refresh_team_hint_enablements(&app.db, team_id, None).await?;
        }
        for puzzle_id in puzzles {
            db::cache::del_pattern(&app.kv, &format!("puzzle:{puzzle_id}:team:*:full_state"))
                .await?;
        }
        for round_id in rounds {
            db::cache::del_pattern(&app.kv, &format!("round:{round_id}:team:*:full_state")).await?;
        }
        app.sync_hub
            .notify_game_release_updated(event.game_id, event.id);
        db::release::mark_notified(&app.db, event.id).await?;
    }
    Ok(())
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

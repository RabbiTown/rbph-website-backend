use std::time::Duration;

use crate::{AppState, db, error::RbInternalError};

const MIN_REFRESH_INTERVAL_SECONDS: i32 = 1;
const MAX_REFRESH_INTERVAL_SECONDS: i32 = 86_400;

pub fn normalize_refresh_interval(seconds: i32) -> i32 {
    seconds.clamp(MIN_REFRESH_INTERVAL_SECONDS, MAX_REFRESH_INTERVAL_SECONDS)
}

pub async fn process_dirty_leaderboards(app: &AppState) -> Result<(), RbInternalError> {
    for game_id in db::board::LEADER_BOARD_CACHE.dirty_games(&app.db).await? {
        if let Err(error) = db::board::LEADER_BOARD_CACHE
            .refresh_game(&app.db, &app.kv, game_id)
            .await
        {
            log::error!("failed to refresh leaderboard for game {game_id}: {error}");
        }
    }
    Ok(())
}

pub async fn run_scheduler(app: AppState) {
    loop {
        let seconds = normalize_refresh_interval(
            app.system_settings
                .read()
                .await
                .leaderboard_refresh_interval_seconds,
        );
        tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
        if let Err(error) = process_dirty_leaderboards(&app).await {
            log::error!("leaderboard scheduler failed: {error}");
        }
    }
}

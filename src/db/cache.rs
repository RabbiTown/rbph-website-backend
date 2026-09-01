use deadpool_redis::redis::{self, AsyncCommands, RedisError};

use crate::{AppState, KvPool, db, error::RbInternalError};

pub async fn del_pattern(kv_pool: &KvPool, pattern: &str) -> Result<(), RbInternalError> {
    let mut conn = kv_pool.get().await?;

    let mut cursor: u64 = 0;

    loop {
        let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .cursor_arg(cursor)
            .arg("MATCH")
            .arg(pattern)
            .arg("COUNT")
            .arg(100)
            .query_async(&mut *conn)
            .await?;

        cursor = new_cursor;

        if !keys.is_empty() {
            let _: () = conn.unlink(keys).await?;
        }

        if cursor == 0 {
            break;
        }
    }

    Ok(())
}

macro_rules! invalidate_cache {
    ($kv_pool:expr, keys = [$($key:expr),*], patterns = [$($pat:expr),*]) => {{
        let keys = [$($key),*];
        let patterns = [$($pat),*];

        let kv_pool = $kv_pool.clone();
        tokio::spawn(async move {
            let mut conn = kv_pool.get().await.unwrap();
            let _: Result<(), RedisError> = conn.del(&keys).await;

            for pattern in patterns {
                let _ = db::cache::del_pattern(&kv_pool, &pattern).await;
            }
        });
    }};

    ($kv_pool:expr, keys = [$($key:expr),*]) => {{
        let keys: Vec<String> = vec![$($key.to_string()),*];

        let kv_pool = $kv_pool.clone();
        tokio::spawn(async move {
            let mut conn = kv_pool.get().await.unwrap();
            let _: Result<(), RedisError> = conn.del(&keys).await;
        });
    }};
}

pub async fn invalidate_team_info(app: &AppState, team_id: i32) -> Result<(), RbInternalError> {
    db::board::LEADER_BOARD_CACHE
        .update_team(&app.db, team_id, false)
        .await?;

    let _ = app
        .sync_hub
        .notify_team_info_updated(&app.db, team_id)
        .await;

    Ok(())
}

pub async fn remove_team_info(app: &AppState, game_id: i32) -> Result<(), RbInternalError> {
    db::board::LEADER_BOARD_CACHE
        .remove_team(&app.db, game_id)
        .await?;

    Ok(())
}

pub async fn invalidate_team_hints(
    app: &AppState,
    team_id: i32,
    puzzle_id: i32,
) -> Result<(), RbInternalError> {
    invalidate_cache!(
        app.kv,
        keys = [format!("puzzle:{puzzle_id}:team:{team_id}:hints")]
    );

    Ok(())
}

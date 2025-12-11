use deadpool_redis::redis::{self, AsyncCommands};

use crate::{KvPool, db, error::RbInternalError};

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

pub async fn invalidate_team_puzzle(
    kv_pool: &KvPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<(), RbInternalError> {
    db::puzzle::invalidate_puzzle_state_cache(kv_pool, team_id, puzzle_id).await?;
    db::round::invalidate_puzzles_for_team_cache(kv_pool, team_id).await?;

    Ok(())
}

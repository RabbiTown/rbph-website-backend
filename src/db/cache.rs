use deadpool_redis::redis::{self, AsyncCommands, RedisError};

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
    let keys = [
        format!("puzzle:{puzzle_id}:team:{team_id}:state"),
        format!("puzzle:{puzzle_id}:team:{team_id}:full_state"),
    ];

    let patterns = [format!("round:*:team:{team_id}:puzzles")];

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.del(&keys).await;

        for pattern in patterns {
            let _ = db::cache::del_pattern(&kv_pool, &pattern).await;
        }
    });

    Ok(())
}

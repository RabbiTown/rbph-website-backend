use deadpool_redis::redis::{self, AsyncCommands, RedisError};

use crate::{AppState, db, error::RbInternalError, kv::KvStore};

pub async fn del_pattern(kv_pool: &KvStore, pattern: &str) -> Result<(), RbInternalError> {
    let mut conn = kv_pool.get().await?;

    let mut cursor: u64 = 0;

    loop {
        let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .cursor_arg(cursor)
            .arg("MATCH")
            .arg(kv_pool.pattern(pattern))
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
            let keys = keys.map(|key| kv_pool.key(key));
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
            let keys = keys
                .into_iter()
                .map(|key| kv_pool.key(key))
                .collect::<Vec<_>>();
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
        keys = [format!(
            "cache:puzzle-hints:v1:puzzle:{puzzle_id}:team:{team_id}"
        )]
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use deadpool_redis::redis::AsyncCommands;
    use uuid::Uuid;

    use crate::kv::KvStore;

    #[tokio::test]
    #[ignore = "requires RBPH_TEST_REDIS_URL"]
    async fn pattern_deletion_is_limited_to_one_deployment() {
        let redis_url = std::env::var("RBPH_TEST_REDIS_URL")
            .expect("RBPH_TEST_REDIS_URL must be set for ignored Redis integration tests");
        let pool = deadpool_redis::Config::from_url(&redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        let random = Uuid::new_v4().simple().to_string();
        let suffix = &random[..8];
        let first = KvStore::new(pool.clone(), redis_url.clone(), &format!("test-a-{suffix}"));
        let second = KvStore::new(pool, redis_url, &format!("test-b-{suffix}"));
        let first_key = first.key("cache:test:v1:item");
        let second_key = second.key("cache:test:v1:item");

        let mut conn = first.get().await.expect("test Redis should be available");
        let _: () = conn
            .set(&first_key, "first")
            .await
            .expect("first test key should be stored");
        let _: () = conn
            .set(&second_key, "second")
            .await
            .expect("second test key should be stored");
        drop(conn);

        super::del_pattern(&first, "cache:test:v1:*")
            .await
            .expect("pattern deletion should succeed");

        let mut conn = second.get().await.expect("test Redis should be available");
        let first_value: Option<String> = conn.get(&first_key).await.unwrap();
        let second_value: Option<String> = conn.get(&second_key).await.unwrap();
        assert!(first_value.is_none());
        assert_eq!(second_value.as_deref(), Some("second"));
        let _: () = conn.del(second_key).await.unwrap();
    }
}

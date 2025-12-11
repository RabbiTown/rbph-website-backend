use deadpool_redis::redis::{AsyncCommands, RedisError};
use serde::Serialize;

use crate::{
    DbPool, KvPool,
    db::{self, puzzle::GameUserInfo},
    error::RbInternalError,
    model::game::{RbContentType, RbPuzzleType},
};

pub async fn get_round_game(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    round_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{}:game", round_id);

    if let Some(cache) = conn.get(&key).await? {
        return Ok((cache != -1).then(|| cache));
    }

    let result = sqlx::query_scalar!(
        "SELECT game_id FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result.unwrap_or(-1), 60 * 60).await;
    });

    Ok(result)
}

pub async fn get_round_state(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    round_id: i32,
) -> Result<bool, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{round_id}:team:{team_id}:state");

    if let Some(cache) = conn.get::<&str, Option<bool>>(&key).await? {
        return Ok(cache);
    }

    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_team_puzzle tp
            JOIN rb_puzzle p ON p.id = tp.puzzle_id AND p.round_id = $2
            WHERE tp.team_id = $1 AND tp.pstate >= 0
        );",
        team_id,
        round_id
    )
    .fetch_one(db_pool)
    .await?
    .unwrap_or(false);

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result, 60 * 60).await;
    });

    Ok(result)
}

pub async fn get_round_user_info(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    round_id: i32,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let game_id = get_round_game(db_pool, kv_pool, round_id).await?;
    if game_id.is_none() {
        return Ok(None);
    }
    let game_id = game_id.unwrap();

    // TODO : check game is online & in progress

    let team_id = db::team::get_id_by_user_game(db_pool, kv_pool, user_id, game_id).await?;
    if team_id.is_none() {
        return Ok(None);
    }
    let team_id = team_id.unwrap();

    let access = get_round_state(db_pool, kv_pool, team_id, round_id).await?;

    match access {
        true => Ok(Some(GameUserInfo { game_id, team_id })),
        false => Ok(None),
    }
}

#[derive(Serialize)]
pub struct RbRoundShowData {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub content_type: RbContentType,
    pub cover: Option<String>,
    pub game_id: i32,
    pub puzzle: Option<i32>,
}

pub async fn get_info_show(
    db_pool: &DbPool,
    round_id: i32,
) -> Result<Option<RbRoundShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundShowData,
        "SELECT id, title, content, content_type, cover, game_id, puzzle
        FROM rb_round
        WHERE id = $1",
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_info_show_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    round_id: i32,
) -> Result<Option<String>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{round_id}:show");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(Some(cache));
    }

    let result = get_info_show(db_pool, round_id)
        .await?
        .map(|x| serde_json::to_string(&x))
        .transpose()?;

    if result.is_some() {
        let kv_pool = kv_pool.clone();
        let result = result.clone();
        tokio::spawn(async move {
            let mut conn = kv_pool.get().await.unwrap();
            let _: Result<(), RedisError> = conn.set_ex(&key, result, 60 * 60).await;
        });
    }

    Ok(result)
}

#[derive(Serialize)]
pub struct RbPuzzleSimpleData {
    pub id: i32,
    pub title: String,
}

pub async fn get_puzzles_for_team(
    db_pool: &DbPool,
    team_id: i32,
    round_id: i32,
) -> Result<Vec<RbPuzzleSimpleData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbPuzzleSimpleData,
        "SELECT p.id, p.title FROM rb_puzzle p
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
        WHERE p.round_id = $1 AND tp.team_id = $2 AND tp.pstate >= 0
        AND p.id != (SELECT puzzle FROM rb_round WHERE id = $1);",
        round_id,
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_puzzles_for_team_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    round_id: i32,
) -> Result<String, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("round:{round_id}:team:{team_id}:puzzles");

    if let Some(cache) = conn.get(&key).await? {
        return Ok(cache);
    }

    let result = get_puzzles_for_team(db_pool, team_id, round_id).await?;
    let result = serde_json::to_string(&result)?;

    let kv_pool = kv_pool.clone();
    let result_clone = result.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, result_clone, 60 * 60).await;
    });

    Ok(result)
}

pub async fn invalidate_puzzles_for_team_cache(
    kv_pool: &KvPool,
    team_id: i32,
) -> Result<(), RbInternalError> {
    let pattern = format!("round:*:team:{team_id}:puzzles");

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let _ = db::cache::del_pattern(&kv_pool, &pattern).await;
    });

    Ok(())
}

pub async fn get_info_for_team_str(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    round_id: i32,
    team_id: i32,
) -> Result<Option<String>, RbInternalError> {
    if let Some(show_str) = get_info_show_str(db_pool, kv_pool, round_id).await? {
        let puzzles_str = get_puzzles_for_team_str(db_pool, kv_pool, team_id, round_id).await?;
        Ok(Some(format!(
            "{{\"data\":{show_str},\"puzzles\":{puzzles_str}}}"
        )))
    } else {
        Ok(None)
    }
}

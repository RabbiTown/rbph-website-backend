use serde::Serialize;
use sqlx::{QueryBuilder, prelude::FromRow};
use time::OffsetDateTime;

use crate::{
    DbPool, KvPool,
    db::{self},
    error::RbInternalError,
    model::{game::RbGame, user::RbUserRole},
};

// TODO : add kv cache
pub async fn exists(
    pool: &DbPool,
    game_id: i32,
    user_role: RbUserRole,
) -> Result<bool, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT 1 FROM rb_game WHERE id = ");
    qb.push_bind(game_id);

    if user_role != RbUserRole::Admin {
        qb.push(" AND is_shown = true");
    }

    let result = qb
        .build_query_scalar::<i32>()
        .fetch_optional(pool)
        .await?
        .is_some();

    Ok(result)
}

#[derive(FromRow, Serialize)]
pub struct RbGameShowData {
    pub id: i32,
    pub title: String,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub reg_open_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub pre_open_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub start_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub end_at: OffsetDateTime,
    pub cover: Option<String>,
}

pub async fn get_by_id(
    pool: &DbPool,
    game_id: i32,
) -> Result<Option<RbGameShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbGameShowData,
        "SELECT id, title, reg_open_at, pre_open_at, start_at, end_at, cover
        FROM rb_game WHERE id = $1;",
        game_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn list_all(
    pool: &DbPool,
    only_shown: bool,
    only_online: bool,
) -> Result<Vec<RbGame>, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT * FROM rb_game WHERE 1=1");

    if only_shown {
        qb.push(" AND is_shown = true");
    }

    if only_online {
        qb.push(" AND is_online = true");
    }

    let result = qb.build_query_as::<RbGame>().fetch_all(pool).await?;

    Ok(result)
}

#[derive(Clone)]
pub struct GameUserInfo {
    pub game_id: i32,
    pub team_id: Option<i32>,
}

pub async fn get_game_user_info(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    game_id: i32,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    // TODO : check game is online & in progress
    if exists(db_pool, game_id, RbUserRole::User).await? {
        let team_id = db::team::get_id_by_user_game(db_pool, kv_pool, user_id, game_id).await?;
        Ok(Some(GameUserInfo { game_id, team_id }))
    } else {
        Ok(None)
    }
}

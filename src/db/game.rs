use serde::Serialize;
use sqlx::{QueryBuilder, prelude::FromRow};
use time::OffsetDateTime;

use crate::{
    DbPool,
    error::RbInternalError,
    model::{game::RbGame, user::RbUserRole},
};

pub struct RbGamePutData {
    pub title: String,
    pub reg_open_at: Option<OffsetDateTime>,
    pub pre_open_at: Option<OffsetDateTime>,
    pub start_at: OffsetDateTime,
    pub end_at: OffsetDateTime,
}

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
    pub reg_open_at: Option<OffsetDateTime>,
    pub pre_open_at: Option<OffsetDateTime>,
    pub start_at: OffsetDateTime,
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

pub async fn append(pool: &DbPool, data: &RbGamePutData) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_game (title, reg_open_at, pre_open_at, start_at, end_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id;",
        data.title,
        data.reg_open_at,
        data.pre_open_at,
        data.start_at,
        data.end_at
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn list_all(pool: &DbPool, only_shown: bool, only_online: bool) -> Result<Vec<RbGame>, RbInternalError> {
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

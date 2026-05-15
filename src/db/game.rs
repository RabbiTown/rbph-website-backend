use serde::{Deserialize, Deserializer, Serialize};
use sqlx::{QueryBuilder, prelude::FromRow};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    DbPool,
    db::{self},
    error::RbInternalError,
    model::{game::RbGame, user::RbUserRole},
};

#[derive(Deserialize)]
pub struct RbGameCreateData {
    pub title: String,
    pub cover: Option<String>,
    #[serde(default)]
    pub is_shown: bool,
    #[serde(default)]
    pub is_online: bool,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    pub reg_open_at: Option<OffsetDateTime>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    pub pre_open_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub start_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub end_at: OffsetDateTime,
}

#[derive(Default, Deserialize)]
pub struct RbGameUpdateData {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch")]
    pub cover: Option<Option<String>>,
    pub is_shown: Option<bool>,
    pub is_online: Option<bool>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_offset_datetime_patch"
    )]
    pub reg_open_at: Option<Option<OffsetDateTime>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_offset_datetime_patch"
    )]
    pub pre_open_at: Option<Option<OffsetDateTime>>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    pub start_at: Option<OffsetDateTime>,
    #[serde(
        default,
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    pub end_at: Option<OffsetDateTime>,
}

fn deserialize_nullable_string_patch<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

fn deserialize_nullable_offset_datetime_patch<'de, D>(
    deserializer: D,
) -> Result<Option<Option<OffsetDateTime>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|s| OffsetDateTime::parse(&s, &Rfc3339).map_err(serde::de::Error::custom))
        .transpose()
        .map(Some)
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

pub async fn get_full_by_id(
    pool: &DbPool,
    game_id: i32,
) -> Result<Option<RbGame>, RbInternalError> {
    let result = sqlx::query_as!(
        RbGame,
        "SELECT id, title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at, ctime_at
        FROM rb_game WHERE id = $1;",
        game_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn create(pool: &DbPool, data: &RbGameCreateData) -> Result<RbGame, RbInternalError> {
    let result = sqlx::query_as!(
        RbGame,
        "INSERT INTO rb_game (
            title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at, ctime_at;",
        data.title,
        data.cover,
        data.is_shown,
        data.is_online,
        data.reg_open_at,
        data.pre_open_at,
        data.start_at,
        data.end_at
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn update(
    pool: &DbPool,
    game_id: i32,
    data: &RbGameUpdateData,
) -> Result<Option<RbGame>, RbInternalError> {
    let cover_is_set = data.cover.is_some();
    let cover = data.cover.clone().flatten();
    let reg_open_at_is_set = data.reg_open_at.is_some();
    let reg_open_at = data.reg_open_at.flatten();
    let pre_open_at_is_set = data.pre_open_at.is_some();
    let pre_open_at = data.pre_open_at.flatten();

    let result = sqlx::query_as!(
        RbGame,
        "UPDATE rb_game
        SET title = COALESCE($2, title),
            cover = CASE WHEN $3 THEN $4 ELSE cover END,
            is_shown = COALESCE($5, is_shown),
            is_online = COALESCE($6, is_online),
            reg_open_at = CASE WHEN $7 THEN $8 ELSE reg_open_at END,
            pre_open_at = CASE WHEN $9 THEN $10 ELSE pre_open_at END,
            start_at = COALESCE($11, start_at),
            end_at = COALESCE($12, end_at)
        WHERE id = $1
        RETURNING id, title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at, ctime_at;",
        game_id,
        data.title,
        cover_is_set,
        cover,
        data.is_shown,
        data.is_online,
        reg_open_at_is_set,
        reg_open_at,
        pre_open_at_is_set,
        pre_open_at,
        data.start_at,
        data.end_at
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
    #[allow(dead_code)]
    pub game_id: i32,
    pub team_id: Option<i32>,
}

pub async fn get_game_user_info(
    db_pool: &DbPool,
    user_id: i32,
    game_id: i32,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    // TODO : check game is online & in progress
    if exists(db_pool, game_id, RbUserRole::User).await? {
        let team_id = db::team::get_id_by_user_game(db_pool, user_id, game_id).await?;
        Ok(Some(GameUserInfo { game_id, team_id }))
    } else {
        Ok(None)
    }
}

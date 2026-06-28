use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{QueryBuilder, prelude::FromRow};
use time::OffsetDateTime;

use crate::{
    DbPool,
    db::{self},
    error::RbInternalError,
    model::{
        game::{GameSettingGroup, RbGame, RbGameSettings, RbGameTeamSettings},
        user::RbUserRole,
    },
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
    pub settings: Option<Value>,
}

#[derive(Default, Deserialize)]
pub struct RbGameUpdateData {
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub cover: Option<Option<String>>,
    pub is_shown: Option<bool>,
    pub is_online: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_offset_datetime_patch"
    )]
    pub reg_open_at: Option<Option<OffsetDateTime>>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_offset_datetime_patch"
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
    pub settings: Option<Value>,
}

#[derive(Serialize)]
pub struct RbCurrencyAdminData {
    pub id: i32,
    pub name: String,
    pub slug: String,
    pub growth: i64,
    pub init_amount: i64,
    pub init_hidden: bool,
    pub prec: i32,
    pub max_amount: i64,
}

#[derive(Deserialize)]
pub struct RbCurrencyCreateData {
    pub name: String,
    pub slug: String,
    pub growth: i64,
    pub init_amount: i64,
    pub init_hidden: bool,
    pub prec: i32,
    pub max_amount: i64,
}

#[derive(Deserialize)]
pub struct RbCurrencyUpdateData {
    pub name: String,
    pub slug: String,
    pub growth: i64,
    pub init_amount: i64,
    pub init_hidden: bool,
    pub prec: i32,
    pub max_amount: i64,
}

pub fn valid_currency_slug(slug: &str) -> bool {
    let len = slug.len();
    (1..=40).contains(&len)
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn valid_currency_data(
    name: &str,
    slug: &str,
    prec: i32,
    init_amount: i64,
    max_amount: i64,
) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 40
        && valid_currency_slug(slug)
        && (0..=6).contains(&prec)
        && init_amount >= 0
        && max_amount >= 0
        && init_amount <= max_amount
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
            reg_open_at, pre_open_at, start_at, end_at,
            settings AS \"settings: RbGameSettings\", ctime_at
        FROM rb_game WHERE id = $1;",
        game_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn list_currency(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<RbCurrencyAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyAdminData,
        "SELECT id, cname AS name, slug, growth, init_amount, init_hidden, prec, max_amount
        FROM rb_currency
        WHERE game_id = $1
        ORDER BY id;",
        game_id
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn get_currency(
    pool: &DbPool,
    game_id: i32,
    currency_id: i32,
) -> Result<Option<RbCurrencyAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyAdminData,
        "SELECT id, cname AS name, slug, growth, init_amount, init_hidden, prec, max_amount
        FROM rb_currency
        WHERE id = $1 AND game_id = $2;",
        currency_id,
        game_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn currency_belongs_to_game(
    pool: &DbPool,
    game_id: i32,
    currency_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_currency
            WHERE id = $1 AND game_id = $2
        );",
        currency_id,
        game_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result.unwrap_or(false))
}

pub async fn create_currency(
    pool: &DbPool,
    game_id: i32,
    data: &RbCurrencyCreateData,
) -> Result<Option<RbCurrencyAdminData>, RbInternalError> {
    let mut tx = pool.begin().await?;

    let Some(currency) = sqlx::query_as!(
        RbCurrencyAdminData,
        "INSERT INTO rb_currency (cname, slug, growth, init_amount, init_hidden, prec, max_amount, game_id)
        SELECT $2, $3, $4, $5, $6, $7, $8, g.id
        FROM rb_game g
        WHERE g.id = $1
        RETURNING id, cname AS name, slug, growth, init_amount, init_hidden, prec, max_amount;",
        game_id,
        data.name.trim(),
        data.slug.trim(),
        data.growth,
        data.init_amount,
        data.init_hidden,
        data.prec,
        data.max_amount
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.commit().await?;
        return Ok(None);
    };

    sqlx::query!(
        "INSERT INTO rb_team_currency (team_id, currency_id, amount, hidden)
        SELECT id, $2, $3, $4 FROM rb_team
        WHERE game_id = $1
        ON CONFLICT (team_id, currency_id) DO NOTHING;",
        game_id,
        currency.id,
        currency.init_amount,
        currency.init_hidden
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Some(currency))
}

pub async fn update_currency(
    pool: &DbPool,
    game_id: i32,
    currency_id: i32,
    data: &RbCurrencyUpdateData,
) -> Result<Option<RbCurrencyAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyAdminData,
        "UPDATE rb_currency
        SET cname = $3,
            slug = $4,
            growth = $5,
            init_amount = $6,
            init_hidden = $7,
            prec = $8,
            max_amount = $9
        WHERE id = $1 AND game_id = $2
        RETURNING id, cname AS name, slug, growth, init_amount, init_hidden, prec, max_amount;",
        currency_id,
        game_id,
        data.name.trim(),
        data.slug.trim(),
        data.growth,
        data.init_amount,
        data.init_hidden,
        data.prec,
        data.max_amount
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn delete_currency(
    pool: &DbPool,
    game_id: i32,
    currency_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_currency
        WHERE id = $1 AND game_id = $2;",
        currency_id,
        game_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn create(pool: &DbPool, data: &RbGameCreateData) -> Result<RbGame, RbInternalError> {
    let patch = data.settings.clone().unwrap_or(Value::Null);
    let settings = RbGameSettings::sanitize_storage(Some(RbGameSettings::merge_settings_patch(
        RbGameSettings::default_value(),
        patch,
    )));

    let result = sqlx::query_as!(
        RbGame,
        "INSERT INTO rb_game (
            title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at, settings
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at,
            settings AS \"settings: RbGameSettings\", ctime_at;",
        data.title,
        data.cover,
        data.is_shown,
        data.is_online,
        data.reg_open_at,
        data.pre_open_at,
        data.start_at,
        data.end_at,
        settings
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
    let settings_is_set = data.settings.is_some();
    let settings = if let Some(patch) = data.settings.clone() {
        let current = sqlx::query_scalar!("SELECT settings FROM rb_game WHERE id = $1;", game_id)
            .fetch_optional(pool)
            .await?;

        let Some(current) = current else {
            return Ok(None);
        };

        Some(RbGameSettings::sanitize_storage(Some(
            RbGameSettings::merge_settings_patch(current, patch),
        )))
    } else {
        None
    };

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
            end_at = COALESCE($12, end_at),
            settings = CASE WHEN $13 THEN $14 ELSE settings END
        WHERE id = $1
        RETURNING id, title, cover, is_shown, is_online,
            reg_open_at, pre_open_at, start_at, end_at,
            settings AS \"settings: RbGameSettings\", ctime_at;",
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
        data.end_at,
        settings_is_set,
        settings
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

    let result = qb
        .push(" ORDER BY id")
        .build_query_as::<RbGame>()
        .fetch_all(pool)
        .await?;

    Ok(result)
}

pub async fn list_show(
    pool: &DbPool,
    only_shown: bool,
    only_online: bool,
) -> Result<Vec<RbGameShowData>, RbInternalError> {
    let mut qb = QueryBuilder::new(
        "SELECT id, title, reg_open_at, pre_open_at, start_at, end_at, cover
        FROM rb_game WHERE 1=1",
    );

    if only_shown {
        qb.push(" AND is_shown = true");
    }

    if only_online {
        qb.push(" AND is_online = true");
    }

    let result = qb
        .push(" ORDER BY id")
        .build_query_as::<RbGameShowData>()
        .fetch_all(pool)
        .await?;

    Ok(result)
}

pub async fn get_team_max_members(
    pool: &DbPool,
    game_id: i32,
) -> Result<Option<Option<i32>>, RbInternalError> {
    Ok(get_setting_group::<RbGameTeamSettings>(pool, game_id)
        .await?
        .map(|settings| settings.max_members))
}

pub async fn get_setting_group<T>(pool: &DbPool, game_id: i32) -> Result<Option<T>, RbInternalError>
where
    T: GameSettingGroup,
{
    let path: Vec<String> = T::PATH.iter().map(|key| key.to_string()).collect();
    let value = sqlx::query_scalar::<_, Option<Value>>(
        "SELECT settings #> $2
        FROM rb_game
        WHERE id = $1;",
    )
    .bind(game_id)
    .bind(path)
    .fetch_optional(pool)
    .await?;

    Ok(value.map(|value| decode_setting_group::<T>(value.unwrap_or(Value::Null))))
}

fn decode_setting_group<T>(value: Value) -> T
where
    T: GameSettingGroup,
{
    let default = serde_json::to_value(T::default()).unwrap_or(Value::Object(Map::new()));
    let merged = RbGameSettings::merge_patch(default, value);

    serde_json::from_value::<T>(merged)
        .unwrap_or_default()
        .sanitize()
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

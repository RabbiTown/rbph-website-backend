use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{QueryBuilder, prelude::FromRow};

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
    pub is_listed: bool,
    #[serde(default)]
    pub is_active: bool,
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
    pub is_listed: Option<bool>,
    pub is_active: Option<bool>,
    pub settings: Option<Value>,
}

pub fn valid_game_title(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty() && title.chars().count() <= 60
}

pub fn game_accessible_for_role(is_active: bool, role: RbUserRole) -> bool {
    is_active || role.is_admin()
}

fn game_accessible_for_user(is_active: bool, role: RbUserRole, is_beta: bool) -> bool {
    game_accessible_for_role(is_active, role) || is_beta
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

#[derive(Serialize)]
pub struct RbAdminPageTitle {
    pub id: i32,
    pub title: String,
}

pub async fn list_admin_page_titles(
    pool: &DbPool,
    game_id: i32,
) -> Result<(Vec<RbAdminPageTitle>, Vec<RbAdminPageTitle>), RbInternalError> {
    let rounds = sqlx::query_as!(
        RbAdminPageTitle,
        "SELECT id, title FROM rb_round WHERE game_id = $1 ORDER BY sort, id",
        game_id,
    )
    .fetch_all(pool);
    let puzzles = sqlx::query_as!(
        RbAdminPageTitle,
        "SELECT p.id, p.title
         FROM rb_puzzle p
         JOIN rb_round r ON r.id = p.round_id
         WHERE r.game_id = $1
         ORDER BY r.sort, r.id, (p.id IS DISTINCT FROM r.puzzle), p.sort, p.id",
        game_id,
    )
    .fetch_all(pool);

    Ok(tokio::try_join!(rounds, puzzles)?)
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
    let result = sqlx::query_scalar!("SELECT is_active FROM rb_game WHERE id = $1", game_id)
        .fetch_optional(pool)
        .await?;

    Ok(result.is_some_and(|is_active| game_accessible_for_role(is_active, user_role)))
}

#[derive(FromRow, Serialize)]
pub struct RbGameShowData {
    pub id: i32,
    pub title: String,
    pub cover: Option<String>,
}

pub async fn get_by_id(
    pool: &DbPool,
    game_id: i32,
) -> Result<Option<RbGameShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbGameShowData,
        "SELECT id, title, cover FROM rb_game WHERE id = $1;",
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
        "SELECT id, title, cover, is_listed, is_active,
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

    sqlx::query_scalar!(
        "SELECT state FROM rb_game_feature
        WHERE game_id = $1 AND feature_type = 4
        FOR UPDATE;",
        game_id
    )
    .fetch_optional(&mut *tx)
    .await?;

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
        SELECT t.id, $2, $3, $4 FROM rb_team t
        WHERE t.game_id = $1
            AND EXISTS (
                SELECT 1 FROM rb_submission s
                WHERE s.team_id = t.id AND s.saction = 3
            )
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
    let mut tx = pool.begin().await?;
    let current = sqlx::query!(
        "SELECT c.growth, gf.state
        FROM rb_currency c
        JOIN rb_game_feature gf ON gf.game_id = c.game_id AND gf.feature_type = 4
        WHERE c.id = $1 AND c.game_id = $2
        FOR UPDATE OF c, gf;",
        currency_id,
        game_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        tx.commit().await?;
        return Ok(None);
    };

    if current.growth != data.growth && current.state == 1 {
        db::feature::settle_currency_growth_conn(
            &mut tx,
            game_id,
            Some(currency_id),
            time::OffsetDateTime::now_utc(),
        )
        .await?;
    }

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
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;
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

    let mut tx = pool.begin().await?;
    let game_id = sqlx::query_scalar!(
        "INSERT INTO rb_game (title, cover, is_listed, is_active, settings)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id;",
        data.title,
        data.cover,
        data.is_listed,
        data.is_active,
        settings
    )
    .fetch_one(&mut *tx)
    .await?;

    db::feature::initialize_game_conn(&mut tx, game_id).await?;

    tx.commit().await?;

    get_full_by_id(pool, game_id)
        .await?
        .ok_or_else(|| "Created game not found".into())
}

pub async fn update(
    pool: &DbPool,
    game_id: i32,
    data: &RbGameUpdateData,
) -> Result<Option<RbGame>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let cover_is_set = data.cover.is_some();
    let cover = data.cover.clone().flatten();
    let settings_is_set = data.settings.is_some();
    let settings = if let Some(patch) = data.settings.clone() {
        let current = sqlx::query_scalar!("SELECT settings FROM rb_game WHERE id = $1;", game_id)
            .fetch_optional(&mut *tx)
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

    let updated = sqlx::query_scalar!(
        "UPDATE rb_game
        SET title = COALESCE($2, title),
            cover = CASE WHEN $3 THEN $4 ELSE cover END,
            is_listed = COALESCE($5, is_listed),
            is_active = COALESCE($6, is_active),
            settings = CASE WHEN $7 THEN $8 ELSE settings END
        WHERE id = $1
        RETURNING id;",
        game_id,
        data.title,
        cover_is_set,
        cover,
        data.is_listed,
        data.is_active,
        settings_is_set,
        settings
    )
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;
    match updated {
        Some(_) => get_full_by_id(pool, game_id).await,
        None => Ok(None),
    }
}

pub async fn list_all(
    pool: &DbPool,
    only_listed: bool,
    only_active: bool,
) -> Result<Vec<RbGame>, RbInternalError> {
    let mut qb = QueryBuilder::new(
        "SELECT g.id, g.title, g.cover, g.is_listed, g.is_active,
            g.settings, g.ctime_at FROM rb_game g WHERE 1=1",
    );

    if only_listed {
        qb.push(" AND g.is_listed = true");
    }

    if only_active {
        qb.push(" AND g.is_active = true");
    }

    let result = qb
        .push(" ORDER BY g.id")
        .build_query_as::<RbGame>()
        .fetch_all(pool)
        .await?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use crate::model::user::RbUserRole;

    use super::{game_accessible_for_role, game_accessible_for_user, valid_game_title};

    #[test]
    fn validates_game_title_length() {
        assert!(!valid_game_title(""));
        assert!(!valid_game_title("   "));
        assert!(valid_game_title("Game"));
        assert!(valid_game_title(&"a".repeat(60)));
        assert!(!valid_game_title(&"a".repeat(61)));
    }

    #[test]
    fn active_games_are_accessible_to_players() {
        assert!(game_accessible_for_role(true, RbUserRole::User));
        assert!(game_accessible_for_role(true, RbUserRole::Moderator));
    }

    #[test]
    fn only_admins_can_access_inactive_games() {
        assert!(!game_accessible_for_role(false, RbUserRole::User));
        assert!(!game_accessible_for_role(false, RbUserRole::Moderator));
        assert!(game_accessible_for_role(false, RbUserRole::Admin));
    }

    #[test]
    fn beta_team_members_can_access_inactive_games() {
        assert!(game_accessible_for_user(false, RbUserRole::User, true));
        assert!(game_accessible_for_user(false, RbUserRole::Moderator, true));
        assert!(!game_accessible_for_user(false, RbUserRole::User, false));
    }
}

pub async fn list_show(
    pool: &DbPool,
    only_listed: bool,
    only_active: bool,
) -> Result<Vec<RbGameShowData>, RbInternalError> {
    let mut qb = QueryBuilder::new("SELECT g.id, g.title, g.cover FROM rb_game g WHERE 1=1");

    if only_listed {
        qb.push(" AND g.is_listed = true");
    }

    if only_active {
        qb.push(" AND g.is_active = true");
    }

    let result = qb
        .push(" ORDER BY g.id")
        .build_query_as::<RbGameShowData>()
        .fetch_all(pool)
        .await?;

    Ok(result)
}

pub async fn list_accessible_show(
    pool: &DbPool,
    user_id: Option<i32>,
) -> Result<Vec<RbGameShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbGameShowData,
        "SELECT DISTINCT g.id, g.title, g.cover
        FROM rb_game g
        LEFT JOIN rb_team_member tm
            ON tm.game_id = g.id AND tm.user_id = $1
        LEFT JOIN rb_team t ON t.id = tm.team_id
        WHERE (g.is_listed AND g.is_active)
            OR ($1::INT IS NOT NULL AND COALESCE(t.is_beta, FALSE))
        ORDER BY g.id;",
        user_id
    )
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
    let value = sqlx::query_scalar!(
        "SELECT settings #> $2
        FROM rb_game
        WHERE id = $1;",
        game_id,
        &path
    )
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
    user_role: RbUserRole,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let row = sqlx::query!(
        "SELECT g.is_active,
            tm.team_id AS \"team_id?\",
            COALESCE(t.is_beta, FALSE) AS \"is_beta!\"
        FROM rb_game g
        LEFT JOIN rb_team_member tm
            ON tm.game_id = g.id AND tm.user_id = $2
        LEFT JOIN rb_team t ON t.id = tm.team_id
        WHERE g.id = $1;",
        game_id,
        user_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(row.and_then(|row| {
        game_accessible_for_user(row.is_active, user_role, row.is_beta).then_some(GameUserInfo {
            game_id,
            team_id: row.team_id,
        })
    }))
}

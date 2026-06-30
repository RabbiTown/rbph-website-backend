use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_repr::Serialize_repr;
use sqlx::QueryBuilder;
use time::OffsetDateTime;
use validator::Validate;

use crate::{
    AppState, DbPool, db,
    error::RbInternalError,
    model::game::{RbTeam, RbTeamState},
};

#[derive(Deserialize)]
pub struct RbTeamPutData {
    pub name: String,
    pub pass: String,
    pub bio: String,
    pub game_id: i32,
}

pub async fn get_id_by_user_game(
    db_pool: &DbPool,
    user_id: i32,
    game_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT team_id FROM rb_team_member
        WHERE user_id = $1 AND game_id = $2;",
        user_id,
        game_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

#[derive(Serialize)]
pub struct RbTeamFullData {
    pub id: i32,
    pub name: String,
    pub state: RbTeamState,
    pub pass: String,
    pub bio: String,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    pub members: Vec<RbTeamMemberData>,
}

#[derive(Serialize)]
pub struct RbTeamMemberData {
    pub id: i32,
    pub is_captain: bool,
    pub nickname: String,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

pub async fn get_by_user_game(
    pool: &DbPool,
    user_id: i32,
    game_id: i32,
) -> Result<Option<RbTeamFullData>, RbInternalError> {
    let team = sqlx::query_as!(
        RbTeam,
        "SELECT t.* FROM rb_team t
        JOIN rb_team_member m ON m.team_id = t.id
        WHERE m.user_id = $1 AND t.game_id = $2;",
        user_id,
        game_id
    )
    .fetch_optional(pool)
    .await?;

    if team.is_none() {
        return Ok(None);
    }

    let team = team.unwrap();

    let members = sqlx::query_as!(
        RbTeamMemberData,
        "SELECT u.id, m.is_captain, u.nickname, m.ctime_at
        FROM rb_team_member m
        JOIN rb_user u ON u.id = m.user_id
        WHERE m.team_id = $1",
        team.id
    )
    .fetch_all(pool)
    .await?;

    Ok(Some(RbTeamFullData {
        id: team.id,
        name: team.name,
        state: team.state,
        pass: team.pass,
        bio: team.bio,
        ctime_at: team.ctime_at,
        members,
    }))
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TeamJoinResult {
    NotFound = -104,
    NotOpen = -5,
    WrongPwd = -4,
    TeamFull = -3,
    Locked = -2,
    ToMany = -1,
    Ok = 0,
}

pub async fn join(
    app: &AppState,
    team_id: i32,
    user_id: i32,
    password: &str,
) -> Result<TeamJoinResult, RbInternalError> {
    let mut tx = app.db.begin().await?;

    let verify = sqlx::query!(
        "SELECT t.state, t.pass, t.game_id,
            COALESCE((SELECT gf.state = 1 FROM rb_game_feature gf
                WHERE gf.game_id = t.game_id AND gf.feature_type = 0), TRUE) AS \"team_open!\"
        FROM rb_team t
        WHERE id = $1
        FOR UPDATE;",
        team_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    if verify.is_none() {
        return Ok(TeamJoinResult::NotFound);
    }
    let verify = verify.unwrap();

    if !verify.team_open {
        return Ok(TeamJoinResult::NotOpen);
    }

    if verify.state == i16::from(RbTeamState::Banned) {
        return Ok(TeamJoinResult::Locked);
    }

    if verify.pass != password {
        return Ok(TeamJoinResult::WrongPwd);
    }

    let max_members = db::game::get_team_max_members(&app.db, verify.game_id)
        .await?
        .ok_or("Game not found")?;

    if let Some(max_members) = max_members {
        let member_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM rb_team_member
            WHERE team_id = $1;",
            team_id
        )
        .fetch_one(&mut *tx)
        .await?
        .unwrap_or(0);

        if member_count >= i64::from(max_members) {
            return Ok(TeamJoinResult::TeamFull);
        }
    }

    let result = sqlx::query!(
        "INSERT INTO rb_team_member (team_id, user_id, is_captain)
        VALUES ($1, $2, FALSE)",
        team_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() > 0 {
        db::event_log::insert_tx(
            &mut tx,
            db::event_log::EventLogInput {
                event_type: "team.member.joined",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Info),
                game_id: Some(verify.game_id),
                team_id: Some(team_id),
                user_id: Some(user_id),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;

        tx.commit().await?;

        // all member => TeamInfoUpdated
        db::cache::invalidate_team_info(app, team_id).await?;

        Ok(TeamJoinResult::Ok)
    } else {
        Ok(TeamJoinResult::ToMany)
    }
}

pub enum TeamCreateResult {
    NotOpen,
    ToMany,
    Ok(i32),
}

pub async fn user_create(
    db_pool: &DbPool,
    user_id: i32,
    data: &RbTeamPutData,
) -> Result<TeamCreateResult, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let team_open = sqlx::query_scalar!(
        "SELECT COALESCE((SELECT gf.state = 1 FROM rb_game_feature gf
            WHERE gf.game_id = g.id AND gf.feature_type = 0), TRUE) AS \"team_open!\"
        FROM rb_game g WHERE g.id = $1;",
        data.game_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if !team_open {
        return Ok(TeamCreateResult::NotOpen);
    }

    let team_id = sqlx::query_scalar!(
        "INSERT INTO rb_team (name, pass, bio, game_id)
        VALUES ($1, $2, $3, $4)
        RETURNING id;",
        data.name,
        data.pass,
        data.bio,
        data.game_id
    )
    .fetch_one(&mut *tx)
    .await?;

    let result = sqlx::query!(
        "INSERT INTO rb_team_member (team_id, user_id, is_captain)
        VALUES ($1, $2, TRUE)
        ON CONFLICT (game_id, user_id) DO NOTHING;",
        team_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(TeamCreateResult::ToMany);
    }

    sqlx::query!(
        "INSERT INTO rb_team_puzzle (team_id, puzzle_id)
        SELECT $1 AS team_id, p.id AS puzzle_id
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id AND r.game_id = $2
        WHERE p.unlock_cond = 'default';",
        team_id,
        data.game_id
    )
    .execute(&mut *tx)
    .await?;

    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "team.created",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(data.game_id),
            team_id: Some(team_id),
            user_id: Some(user_id),
            data: json!({
                "team": { "id": team_id, "name": data.name }
            }),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    Ok(TeamCreateResult::Ok(team_id))
}

pub async fn leave(app: &AppState, team_id: i32, user_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "DELETE FROM rb_team_member tm
        USING rb_team t
        WHERE tm.team_id = $1 AND tm.user_id = $2
            AND t.id = tm.team_id AND t.state < 1
        RETURNING t.game_id;",
        team_id,
        user_id
    )
    .fetch_optional(&app.db)
    .await?;

    if let Some(game_id) = result {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "team.member.left",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Info),
                game_id: Some(game_id),
                team_id: Some(team_id),
                user_id: Some(user_id),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;

        // other member => TeamInfoUpdated
        db::cache::invalidate_team_info(app, team_id).await?;

        Ok(true)
    } else {
        Ok(false)
    }
}

pub async fn disband(app: &AppState, team_id: i32) -> Result<bool, RbInternalError> {
    let mut tx = app.db.begin().await?;

    let members = sqlx::query_scalar!(
        "SELECT tm.user_id FROM rb_team_member tm
        JOIN rb_team t ON t.id = tm.team_id
        WHERE tm.team_id = $1 AND t.state < 1;",
        team_id
    )
    .fetch_all(&mut *tx)
    .await?;

    if members.is_empty() {
        return Ok(false);
    }

    let result = sqlx::query_scalar!(
        "DELETE FROM rb_team
        WHERE id = $1
        RETURNING game_id;",
        team_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(game_id) = result {
        db::event_log::insert_tx(
            &mut tx,
            db::event_log::EventLogInput {
                event_type: "team.disbanded",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Warning),
                game_id: Some(game_id),
                team_id: None,
                data: json!({ "team": { "id": team_id } }),
                ..Default::default()
            },
        )
        .await?;

        tx.commit().await?;

        db::cache::remove_team_info(game_id, team_id).await?;

        app.sync_hub.notify_team_disbanded(&members);

        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Deserialize, Validate)]
pub struct UserUpdateData {
    #[validate(length(min = 1, max = 40))]
    pub name: Option<String>,
    #[validate(length(min = 8, max = 32))]
    pub pass: Option<String>,
    #[validate(length(max = 200))]
    pub bio: Option<String>,
}

pub async fn user_update(
    app: &AppState,
    game_id: i32,
    user_id: i32,
    data: &UserUpdateData,
) -> Result<bool, RbInternalError> {
    let mut qb = QueryBuilder::new("UPDATE rb_team SET ");

    let mut first = true;

    if let Some(name) = &data.name {
        if !first {
            qb.push(", ");
        }
        qb.push("name = ").push_bind(name);
        first = false;
    }

    if let Some(pass) = &data.pass {
        if !first {
            qb.push(", ");
        }
        qb.push("pass = ").push_bind(pass);
        first = false;
    }

    if let Some(bio) = &data.bio {
        if !first {
            qb.push(", ");
        }
        qb.push("bio = ").push_bind(bio);
        first = false;
    }

    if first {
        return Ok(false);
    }

    qb.push(
        " WHERE id = (SELECT team_id FROM rb_team_member tm
            WHERE user_id = ",
    )
    .push_bind(user_id)
    .push(" AND game_id = ")
    .push_bind(game_id)
    .push(" AND is_captain) RETURNING id;");

    let result = qb
        .build_query_scalar::<i32>()
        .fetch_optional(&app.db)
        .await?;

    if let Some(team_id) = result {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "team.updated",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Info),
                game_id: Some(game_id),
                team_id: Some(team_id),
                user_id: Some(user_id),
                data: json!({
                    "fields": {
                        "name": data.name.is_some(),
                        "pass": data.pass.is_some(),
                        "bio": data.bio.is_some()
                    }
                }),
                ..Default::default()
            },
        )
        .await?;

        // all member => TeamInfoUpdated
        db::cache::invalidate_team_info(app, team_id).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Serialize)]
pub struct RbTeamShowData {
    pub id: i32,
    pub name: String,
    pub state: RbTeamState,
    pub bio: String,
    pub members: Vec<RbTeamMemberShowData>,
}

#[derive(Serialize)]
pub struct RbTeamMemberShowData {
    pub id: i32,
    pub is_captain: bool,
    pub nickname: String,
}

pub async fn get_by_id_show(
    pool: &DbPool,
    team_id: i32,
) -> Result<Option<RbTeamShowData>, RbInternalError> {
    let team = sqlx::query_as!(RbTeam, "SELECT * FROM rb_team WHERE id = $1;", team_id,)
        .fetch_optional(pool)
        .await?;

    if team.is_none() {
        return Ok(None);
    }

    let team = team.unwrap();

    let members = sqlx::query_as!(
        RbTeamMemberShowData,
        "SELECT u.id, m.is_captain, u.nickname
        FROM rb_team_member m
        JOIN rb_user u ON u.id = m.user_id
        WHERE m.team_id = $1",
        team.id
    )
    .fetch_all(pool)
    .await?;

    Ok(Some(RbTeamShowData {
        id: team.id,
        name: team.name,
        state: team.state,
        bio: team.bio,
        members,
    }))
}

#[derive(Clone, Serialize)]
pub struct RbCurrencyShowData {
    pub id: i32,
    slug: String,
    name: String,
    growth: i64,
    init_amount: i64,
    prec: i32,
    amount: i64,
    current_amount: i64,
    max_amount: i64,
    hidden: bool,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    utime_at: OffsetDateTime,
}

async fn get_currency_info_impl(
    db_pool: &DbPool,
    team_id: i32,
    include_hidden: bool,
) -> Result<Vec<RbCurrencyShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyShowData,
        "SELECT c.id, c.slug, c.cname AS name, c.growth + tc.growth AS \"growth!\",
                c.init_amount, c.prec, tc.amount,
                LEAST(
                    tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )::BIGINT AS \"current_amount!\",
                c.max_amount, tc.hidden, tc.utime_at
        FROM rb_currency c
        JOIN rb_team_currency tc ON tc.currency_id = c.id
        WHERE tc.team_id = $1 AND ($2 OR NOT tc.hidden);",
        team_id,
        include_hidden
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_currency_info(
    db_pool: &DbPool,
    team_id: i32,
) -> Result<Vec<RbCurrencyShowData>, RbInternalError> {
    get_currency_info_impl(db_pool, team_id, false).await
}

pub async fn get_currency_info_all(
    db_pool: &DbPool,
    team_id: i32,
) -> Result<Vec<RbCurrencyShowData>, RbInternalError> {
    get_currency_info_impl(db_pool, team_id, true).await
}

async fn get_currency_info_one_impl(
    db_pool: &DbPool,
    team_id: i32,
    currency_id: i32,
    include_hidden: bool,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyShowData,
        "SELECT c.id, c.slug, c.cname AS name, c.growth + tc.growth AS \"growth!\",
                c.init_amount, c.prec, tc.amount,
                LEAST(
                    tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )::BIGINT AS \"current_amount!\",
                c.max_amount, tc.hidden, tc.utime_at
        FROM rb_currency c
        JOIN rb_team_currency tc ON tc.currency_id = c.id
        WHERE tc.team_id = $1 AND c.id = $2 AND ($3 OR NOT tc.hidden);",
        team_id,
        currency_id,
        include_hidden
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_currency_info_one_all(
    db_pool: &DbPool,
    team_id: i32,
    currency_id: i32,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    get_currency_info_one_impl(db_pool, team_id, currency_id, true).await
}

async fn get_currency_info_one_by_slug_impl(
    db_pool: &DbPool,
    team_id: i32,
    game_id: i32,
    slug: &str,
    include_hidden: bool,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyShowData,
        "SELECT c.id, c.slug, c.cname AS name, c.growth + tc.growth AS \"growth!\",
                c.init_amount, c.prec, tc.amount,
                LEAST(
                    tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )::BIGINT AS \"current_amount!\",
                c.max_amount, tc.hidden, tc.utime_at
        FROM rb_currency c
        JOIN rb_team_currency tc ON tc.currency_id = c.id
        WHERE tc.team_id = $1 AND c.game_id = $2 AND c.slug = $3 AND ($4 OR NOT tc.hidden);",
        team_id,
        game_id,
        slug,
        include_hidden
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_currency_info_one_by_slug_all(
    db_pool: &DbPool,
    team_id: i32,
    game_id: i32,
    slug: &str,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    get_currency_info_one_by_slug_impl(db_pool, team_id, game_id, slug, true).await
}

#[derive(sqlx::FromRow)]
struct CurrencyRuntimeRow {
    id: i32,
    game_id: i32,
    slug: String,
    name: String,
    prec: i32,
    current_amount: i64,
    max_amount: i64,
}

pub struct UpdateCurrencyOptions {
    pub amount: Option<i64>,
    pub growth: Option<i64>,
    pub hidden: Option<bool>,
}

pub struct CurrencyEventContext<'a> {
    pub puzzle_id: Option<i32>,
    pub puzzle_title: Option<&'a str>,
    pub reason: Option<&'a str>,
}

async fn lock_currency_runtime(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    currency_id: i32,
) -> Result<Option<CurrencyRuntimeRow>, RbInternalError> {
    let row = sqlx::query_as!(
        CurrencyRuntimeRow,
        r#"SELECT
            c.id AS "id!",
            c.game_id AS "game_id!",
            c.slug AS "slug!",
            c.cname AS "name!",
            c.prec AS "prec!",
            LEAST(
                tc.amount::NUMERIC
                    + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                c.max_amount::NUMERIC
            )::BIGINT AS "current_amount!",
            c.max_amount AS "max_amount!"
        FROM rb_team_currency tc
        JOIN rb_currency c ON tc.currency_id = c.id
        WHERE tc.team_id = $1 AND tc.currency_id = $2
        FOR UPDATE;"#,
        team_id,
        currency_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row)
}

async fn lock_currency_runtime_by_slug(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    game_id: i32,
    slug: &str,
) -> Result<Option<CurrencyRuntimeRow>, RbInternalError> {
    let row = sqlx::query_as!(
        CurrencyRuntimeRow,
        r#"SELECT
            c.id AS "id!",
            c.game_id AS "game_id!",
            c.slug AS "slug!",
            c.cname AS "name!",
            c.prec AS "prec!",
            LEAST(
                tc.amount::NUMERIC
                    + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                c.max_amount::NUMERIC
            )::BIGINT AS "current_amount!",
            c.max_amount AS "max_amount!"
        FROM rb_team_currency tc
        JOIN rb_currency c ON tc.currency_id = c.id
        WHERE tc.team_id = $1 AND c.game_id = $2 AND c.slug = $3
        FOR UPDATE;"#,
        team_id,
        game_id,
        slug
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row)
}

async fn update_currency_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    row: &CurrencyRuntimeRow,
    options: &UpdateCurrencyOptions,
) -> Result<(), RbInternalError> {
    let next_amount = options
        .amount
        .unwrap_or(row.current_amount)
        .clamp(0, row.max_amount);
    sqlx::query!(
        r#"UPDATE rb_team_currency
        SET amount = CASE
                WHEN $3::BIGINT IS NULL AND $4::BIGINT IS NULL THEN amount
                ELSE $5
            END,
            growth = COALESCE($4, growth),
            hidden = COALESCE($6, hidden),
            utime_at = CASE
                WHEN $3::BIGINT IS NULL AND $4::BIGINT IS NULL THEN utime_at
                ELSE NOW()
            END
        WHERE team_id = $1 AND currency_id = $2;"#,
        team_id,
        row.id,
        options.amount,
        options.growth,
        next_amount,
        options.hidden
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn update_currency(
    db_pool: &DbPool,
    team_id: i32,
    currency_id: i32,
    options: UpdateCurrencyOptions,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime(&mut tx, team_id, currency_id).await? else {
        tx.commit().await?;
        return Ok(None);
    };

    update_currency_tx(&mut tx, team_id, &row, &options).await?;
    tx.commit().await?;

    get_currency_info_one_all(db_pool, team_id, currency_id).await
}

pub async fn update_currency_by_slug(
    db_pool: &DbPool,
    team_id: i32,
    game_id: i32,
    slug: &str,
    options: UpdateCurrencyOptions,
) -> Result<Option<RbCurrencyShowData>, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime_by_slug(&mut tx, team_id, game_id, slug).await? else {
        tx.commit().await?;
        return Ok(None);
    };

    update_currency_tx(&mut tx, team_id, &row, &options).await?;
    let currency_id = row.id;
    tx.commit().await?;

    get_currency_info_one_all(db_pool, team_id, currency_id).await
}

pub async fn cost_currency(
    db_pool: &DbPool,
    team_id: i32,
    currency_id: i32,
    delta: i64,
    context: Option<CurrencyEventContext<'_>>,
) -> Result<bool, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime(&mut tx, team_id, currency_id).await? else {
        tx.commit().await?;
        return Ok(false);
    };

    let Some(next_amount) = row.current_amount.checked_add(delta) else {
        tx.commit().await?;
        return Ok(false);
    };
    if next_amount < 0 || next_amount > row.max_amount {
        tx.commit().await?;
        return Ok(false);
    }

    sqlx::query!(
        r#"UPDATE rb_team_currency
        SET amount = $3, utime_at = NOW()
        WHERE team_id = $1 AND currency_id = $2;"#,
        team_id,
        currency_id,
        next_amount
    )
    .execute(&mut *tx)
    .await?;

    let after = next_amount;
    let context = context.as_ref();
    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "currency.cost",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(row.game_id),
            team_id: Some(team_id),
            currency_id: Some(row.id),
            delta_amount: Some(after - row.current_amount),
            data: db::event_log::CurrencyEventData {
                id: row.id,
                slug: row.slug,
                name: row.name,
                prec: row.prec,
                before: row.current_amount,
                after,
            }
            .json(
                context.and_then(|context| context.reason),
                context.and_then(|context| context.puzzle_id),
                context.and_then(|context| context.puzzle_title),
            ),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    Ok(true)
}

pub async fn cost_currency_by_slug(
    db_pool: &DbPool,
    team_id: i32,
    game_id: i32,
    slug: &str,
    delta: i64,
    context: Option<CurrencyEventContext<'_>>,
) -> Result<bool, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime_by_slug(&mut tx, team_id, game_id, slug).await? else {
        tx.commit().await?;
        return Ok(false);
    };

    let Some(next_amount) = row.current_amount.checked_add(delta) else {
        tx.commit().await?;
        return Ok(false);
    };
    if next_amount < 0 || next_amount > row.max_amount {
        tx.commit().await?;
        return Ok(false);
    }

    sqlx::query!(
        r#"UPDATE rb_team_currency tc
        SET amount = $4, utime_at = NOW()
        FROM rb_currency c
        WHERE tc.currency_id = c.id
            AND tc.team_id = $1
            AND c.game_id = $2
            AND c.slug = $3;"#,
        team_id,
        game_id,
        slug,
        next_amount
    )
    .execute(&mut *tx)
    .await?;

    let after = next_amount;
    let context = context.as_ref();
    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "currency.cost",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(row.game_id),
            team_id: Some(team_id),
            currency_id: Some(row.id),
            delta_amount: Some(after - row.current_amount),
            data: db::event_log::CurrencyEventData {
                id: row.id,
                slug: row.slug,
                name: row.name,
                prec: row.prec,
                before: row.current_amount,
                after,
            }
            .json(
                context.and_then(|context| context.reason),
                context.and_then(|context| context.puzzle_id),
                context.and_then(|context| context.puzzle_title),
            ),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    Ok(true)
}

pub async fn add_currency(
    db_pool: &DbPool,
    team_id: i32,
    currency_id: i32,
    delta: i64,
    context: Option<CurrencyEventContext<'_>>,
) -> Result<Option<i64>, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime(&mut tx, team_id, currency_id).await? else {
        tx.commit().await?;
        return Ok(None);
    };

    let next_amount = row.current_amount.saturating_add(delta);
    let stored_amount = next_amount.clamp(0, row.max_amount);
    let actual_growth = stored_amount - row.current_amount;

    sqlx::query!(
        r#"UPDATE rb_team_currency
        SET amount = $3, utime_at = NOW()
        WHERE team_id = $1 AND currency_id = $2;"#,
        team_id,
        currency_id,
        stored_amount
    )
    .execute(&mut *tx)
    .await?;

    let context = context.as_ref();
    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "currency.added",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(row.game_id),
            team_id: Some(team_id),
            currency_id: Some(row.id),
            delta_amount: Some(actual_growth),
            data: db::event_log::CurrencyEventData {
                id: row.id,
                slug: row.slug,
                name: row.name,
                prec: row.prec,
                before: row.current_amount,
                after: stored_amount,
            }
            .json(
                context.and_then(|context| context.reason),
                context.and_then(|context| context.puzzle_id),
                context.and_then(|context| context.puzzle_title),
            ),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    Ok(Some(actual_growth))
}

pub async fn add_currency_by_slug(
    db_pool: &DbPool,
    team_id: i32,
    game_id: i32,
    slug: &str,
    delta: i64,
    context: Option<CurrencyEventContext<'_>>,
) -> Result<Option<i64>, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let Some(row) = lock_currency_runtime_by_slug(&mut tx, team_id, game_id, slug).await? else {
        tx.commit().await?;
        return Ok(None);
    };

    let next_amount = row.current_amount.saturating_add(delta);
    let stored_amount = next_amount.clamp(0, row.max_amount);
    let actual_growth = stored_amount - row.current_amount;

    sqlx::query!(
        r#"UPDATE rb_team_currency tc
        SET amount = $4, utime_at = NOW()
        FROM rb_currency c
        WHERE tc.currency_id = c.id
            AND tc.team_id = $1
            AND c.game_id = $2
            AND c.slug = $3;"#,
        team_id,
        game_id,
        slug,
        stored_amount
    )
    .execute(&mut *tx)
    .await?;

    let context = context.as_ref();
    db::event_log::insert_tx(
        &mut tx,
        db::event_log::EventLogInput {
            event_type: "currency.added",
            event_scope: i16::from(db::event_log::EventScope::TeamActivity),
            severity: i16::from(db::event_log::EventSeverity::Info),
            game_id: Some(row.game_id),
            team_id: Some(team_id),
            currency_id: Some(row.id),
            delta_amount: Some(actual_growth),
            data: db::event_log::CurrencyEventData {
                id: row.id,
                slug: row.slug,
                name: row.name,
                prec: row.prec,
                before: row.current_amount,
                after: stored_amount,
            }
            .json(
                context.and_then(|context| context.reason),
                context.and_then(|context| context.puzzle_id),
                context.and_then(|context| context.puzzle_title),
            ),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;
    Ok(Some(actual_growth))
}

pub async fn get_member_id(db_pool: &DbPool, team_id: i32) -> Result<Vec<i32>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT user_id
        FROM rb_team_member
        WHERE team_id = $1;",
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
}

pub async fn is_leader_in_game(
    app: &AppState,
    game_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT is_captain FROM rb_team_member
        WHERE game_id = $1 AND user_id = $2;",
        game_id,
        user_id
    )
    .fetch_optional(&app.db)
    .await?
    .unwrap_or(false);

    Ok(result)
}

pub async fn kick_member(
    app: &AppState,
    team_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "DELETE FROM rb_team_member tm
        USING rb_team t
        WHERE tm.team_id = $1 AND tm.user_id = $2 AND t.id = tm.team_id
        RETURNING t.game_id;",
        team_id,
        user_id
    )
    .fetch_optional(&app.db)
    .await?;

    if let Some(game_id) = result {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "team.member.kicked",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Warning),
                game_id: Some(game_id),
                team_id: Some(team_id),
                target_user_id: Some(user_id),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;

        // kicked member => TeamSelfKicked
        // other member => TeamInfoUpdated
        db::cache::invalidate_team_info(app, team_id).await?;

        app.sync_hub.notify_team_self_kicked(user_id);

        Ok(true)
    } else {
        Ok(false)
    }
}

pub async fn promote_member(
    app: &AppState,
    team_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "WITH target AS (
            SELECT game_id
            FROM rb_team_member
            WHERE team_id = $1 AND user_id = $2
        ), updated AS (
            UPDATE rb_team_member
            SET is_captain = (user_id = $2)
            WHERE team_id = $1 AND EXISTS (SELECT 1 FROM target)
            RETURNING 1
        )
        SELECT game_id FROM target;",
        team_id,
        user_id
    )
    .fetch_optional(&app.db)
    .await?;

    if let Some(game_id) = result {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "team.member.promoted",
                event_scope: i16::from(db::event_log::EventScope::TeamActivity),
                severity: i16::from(db::event_log::EventSeverity::Info),
                game_id: Some(game_id),
                team_id: Some(team_id),
                target_user_id: Some(user_id),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;

        // all member => TeamInfoUpdated
        // target member => TeamSelfPromoted
        db::cache::invalidate_team_info(app, team_id).await?;

        app.sync_hub.notify_team_self_promoted(user_id);

        Ok(true)
    } else {
        Ok(false)
    }
}

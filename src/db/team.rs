use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
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
        "SELECT state, pass, game_id FROM rb_team
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

    if verify.state == i16::from(RbTeamState::Banned) {
        return Ok(TeamJoinResult::Locked);
    }

    if verify.pass != password {
        return Ok(TeamJoinResult::WrongPwd);
    }

    let max_members = db::game::get_team_max_members(&app.db, verify.game_id)
        .await?
        .ok_or("Game not found")?;

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

    let result = sqlx::query!(
        "INSERT INTO rb_team_member (team_id, user_id, is_captain)
        VALUES ($1, $2, FALSE)",
        team_id,
        user_id
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() > 0 {
        tx.commit().await?;

        // all member => TeamInfoUpdated
        db::cache::invalidate_team_info(app, team_id).await?;

        Ok(TeamJoinResult::Ok)
    } else {
        Ok(TeamJoinResult::ToMany)
    }
}

pub async fn user_create(
    db_pool: &DbPool,
    user_id: i32,
    data: &RbTeamPutData,
) -> Result<Option<i32>, RbInternalError> {
    let mut tx = db_pool.begin().await?;

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
        return Ok(None);
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

    tx.commit().await?;
    Ok(Some(team_id))
}

pub async fn leave(app: &AppState, team_id: i32, user_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_team_member tm
        USING rb_team t
        WHERE tm.team_id = $1 AND tm.user_id = $2
            AND t.id = tm.team_id AND t.state < 1;",
        team_id,
        user_id
    )
    .execute(&app.db)
    .await?;

    if result.rows_affected() > 0 {
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

#[derive(Serialize)]
pub struct RbCurrencyShowData {
    pub id: i32,
    name: String,
    growth: i32,
    prec: i32,
    amount: i32,
    max_amount: i32,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    utime_at: OffsetDateTime,
}

pub async fn get_currency_info(
    db_pool: &DbPool,
    team_id: i32,
) -> Result<Vec<RbCurrencyShowData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbCurrencyShowData,
        "SELECT c.id, c.cname AS name, c.growth + tc.growth AS \"growth!\",
                c.prec, tc.amount, c.max_amount, tc.utime_at
        FROM rb_currency c
        JOIN rb_team_currency tc ON tc.currency_id = c.id
        WHERE tc.team_id = $1;",
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    Ok(result)
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
        "DELETE FROM rb_team_member
        WHERE team_id = $1 AND user_id = $2;",
        team_id,
        user_id
    )
    .execute(&app.db)
    .await?;

    if result.rows_affected() > 0 {
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
        "UPDATE rb_team_member
        SET is_captain = (user_id = $2)
        WHERE team_id = $1
            AND EXISTS (
                SELECT 1 FROM rb_team_member
                WHERE team_id = $1 AND user_id = $2
            );",
        team_id,
        user_id
    )
    .execute(&app.db)
    .await?;

    if result.rows_affected() > 0 {
        // all member => TeamInfoUpdated
        // target member => TeamSelfPromoted
        db::cache::invalidate_team_info(app, team_id).await?;

        app.sync_hub.notify_team_self_promoted(user_id);

        Ok(true)
    } else {
        Ok(false)
    }
}

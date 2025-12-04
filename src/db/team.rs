use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::{
    DbPool,
    error::RbInternalError,
    model::game::{RbTeam, RbTeamState},
};

#[derive(Deserialize)]
pub struct RbTeamPutData {
    pub tname: String,
    pub pass: String,
    pub bio: String,
    pub game_id: i32,
}

pub async fn append(pool: &DbPool, data: &RbTeamPutData) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_team (tname, pass, bio, game_id)
        VALUES ($1, $2, $3, $4)
        RETURNING id;",
        data.tname,
        data.pass,
        data.bio,
        data.game_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

#[derive(Serialize)]
pub struct RbTeamFullData {
    pub id: i32,
    pub tname: String,
    pub tstate: RbTeamState,
    pub pass: String,
    pub bio: String,
    pub ctime_at: OffsetDateTime,
    pub members: Vec<RbTeamMemberData>,
}

#[derive(Serialize)]
pub struct RbTeamMemberData {
    pub id: i32,
    pub is_captain: bool,
    pub nickname: String,
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
        tname: team.tname,
        tstate: team.tstate,
        pass: team.pass,
        bio: team.bio,
        ctime_at: team.ctime_at,
        members,
    }))
}

#[derive(Serialize)]
pub struct RbTeamVerifyData {
    pub tstate: RbTeamState,
    pub pass: String,
    pub game_id: i32,
    pub member_count: Option<i64>,
}

pub async fn get_by_id_verify(
    pool: &DbPool,
    team_id: i32,
) -> Result<Option<RbTeamVerifyData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbTeamVerifyData,
        "SELECT t.tstate, t.pass, t.game_id, COUNT(m.user_id) AS member_count
        FROM rb_team t
        LEFT JOIN rb_team_member m ON m.team_id = t.id
        WHERE t.id = $1
        GROUP BY t.id",
        team_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn join(
    pool: &DbPool,
    team_id: i32,
    user_id: i32,
    is_captain: bool,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "INSERT INTO rb_team_member (team_id, user_id, is_captain)
        VALUES ($1, $2, $3)
        ON CONFLICT (team_id, user_id) DO NOTHING;",
        team_id,
        user_id,
        is_captain
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn user_create(
    pool: &DbPool,
    user_id: i32,
    data: &RbTeamPutData,
) -> Result<Option<i32>, RbInternalError> {
    let mut tx = pool.begin().await?;

    let team_id = sqlx::query_scalar!(
        "INSERT INTO rb_team (tname, pass, bio, game_id)
        VALUES ($1, $2, $3, $4)
        RETURNING id;",
        data.tname,
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

    if result.rows_affected() > 0 {
        tx.commit().await?;
        Ok(Some(team_id))
    } else {
        Ok(None)
    }
}

pub async fn leave(pool: &DbPool, game_id: i32, user_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_team_member
        WHERE game_id = $1 AND user_id = $2 AND is_captain = FALSE",
        game_id,
        user_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

#[derive(Serialize)]
pub struct RbTeamShowData {
    pub id: i32,
    pub tname: String,
    pub tstate: RbTeamState,
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
        tname: team.tname,
        tstate: team.tstate,
        bio: team.bio,
        members,
    }))
}

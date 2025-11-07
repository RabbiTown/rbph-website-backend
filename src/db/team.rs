use std::result;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError, model::team::RbTeam};

#[derive(Deserialize)]
pub struct RbTeamPutData {
    pub tname: String,
    pub pass: String,
    pub bio: String,
}

pub async fn append(pool: &DbPool, data: &RbTeamPutData) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_team (tname, pass, bio)
        VALUES ($1, $2, $3)
        RETURNING id;",
        data.tname,
        data.pass,
        data.bio,
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn count_user_teams(pool: &DbPool, user_id: i32) -> Result<i64, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rb_team_member WHERE user_id = $1;",
        user_id,
    )
    .fetch_one(pool)
    .await?;

    Ok(result.unwrap_or_default())
}

pub async fn count_members(pool: &DbPool, team_id: i32) -> Result<i64, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rb_team_member WHERE team_id = $1;",
        team_id,
    )
    .fetch_one(pool)
    .await?;

    Ok(result.unwrap_or_default())
}

#[derive(Serialize)]
pub struct RbTeamFullData {
    pub id: i32,
    pub tname: String,
    pub pass: String,
    pub bio: String,
    pub locked: bool,
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

// TODO : better impl for more teams?
pub async fn get_by_user(
    pool: &DbPool,
    user_id: i32,
) -> Result<Vec<RbTeamFullData>, RbInternalError> {
    let teams = sqlx::query_as!(
        RbTeam,
        "SELECT t.* FROM rb_team t
        JOIN rb_team_member m ON m.team_id = t.id
        WHERE m.user_id = $1",
        user_id
    )
    .fetch_all(pool)
    .await?;

    let mut result = Vec::new();
    for team in teams {
        let members = sqlx::query_as!(
            RbTeamMemberData,
            r#"
                SELECT u.id, m.is_captain, u.nickname, m.ctime_at
                FROM rb_team_member m
                JOIN rb_user u ON u.id = m.user_id
                WHERE m.team_id = $1
                "#,
            team.id
        )
        .fetch_all(pool)
        .await?;

        result.push(RbTeamFullData {
            id: team.id,
            tname: team.tname,
            pass: team.pass,
            bio: team.bio,
            locked: team.locked,
            ctime_at: team.ctime_at,
            members,
        });
    }

    Ok(result)
}

#[derive(Serialize)]
pub struct RbTeamVerifyData {
    pub pass: String,
    pub locked: bool,
    pub member_count: Option<i64>,
}

pub async fn get_by_id_verify(
    pool: &DbPool,
    team_id: i32,
) -> Result<Option<RbTeamVerifyData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbTeamVerifyData,
        "SELECT t.pass, t.locked, COUNT(m.user_id) AS member_count
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

pub async fn leave(pool: &DbPool, team_id: i32, user_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_team_member
        WHERE team_id = $1 AND user_id = $2;",
        team_id,
        user_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

use deadpool_redis::redis::{AsyncCommands, RedisError};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;
use time::OffsetDateTime;
use validator::Validate;

use crate::{
    AppState, DbPool, KvPool, db,
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

pub async fn get_id_by_user_game(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    game_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;
    let key = format!("game:{game_id}:user:{user_id}:team_id");

    if let Some(cache) = conn.get(&key).await? {
        return Ok((cache != -1).then_some(cache));
    }

    let result = sqlx::query_scalar!(
        "SELECT team_id FROM rb_team_member
        WHERE user_id = $1 AND game_id = $2;",
        user_id,
        game_id
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

pub async fn update_user_team_cache(
    kv_pool: &KvPool,
    user_id: i32,
    game_id: i32,
    team_id: Option<i32>,
) -> Result<(), RbInternalError> {
    let key = format!("game:{game_id}:user:{user_id}:team_id");

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        let _: Result<(), RedisError> = conn.set_ex(&key, team_id.unwrap_or(-1), 60 * 60).await;
    });

    Ok(())
}

pub async fn update_users_team_cache(
    kv_pool: &KvPool,
    user_ids: &[i32],
    game_id: i32,
    team_id: Option<i32>,
) -> Result<(), RbInternalError> {
    let keys: Vec<String> = user_ids
        .iter()
        .map(|x| format!("game:{game_id}:user:{x}:team_id"))
        .collect();

    let kv_pool = kv_pool.clone();
    tokio::spawn(async move {
        let mut conn = kv_pool.get().await.unwrap();
        for key in keys {
            let _: Result<(), RedisError> = conn.set_ex(&key, team_id.unwrap_or(-1), 60 * 60).await;
        }
    });

    Ok(())
}

#[derive(Serialize)]
pub struct RbTeamFullData {
    pub id: i32,
    pub tname: String,
    pub tstate: RbTeamState,
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
    db_pool: &DbPool,
    kv_pool: &KvPool,
    team_id: i32,
    user_id: i32,
    is_captain: bool,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_team_member (team_id, user_id, is_captain)
        VALUES ($1, $2, $3)
        ON CONFLICT (team_id, user_id) DO NOTHING
        RETURNING game_id;",
        team_id,
        user_id,
        is_captain
    )
    .fetch_optional(db_pool)
    .await?;

    if let Some(game_id) = result {
        update_user_team_cache(kv_pool, user_id, game_id, Some(team_id)).await?;
    }

    Ok(result.is_some())
}

pub async fn user_create(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    user_id: i32,
    data: &RbTeamPutData,
) -> Result<Option<i32>, RbInternalError> {
    let mut tx = db_pool.begin().await?;

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
    update_user_team_cache(kv_pool, user_id, data.game_id, Some(team_id)).await?;
    Ok(Some(team_id))
}

pub async fn leave(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    game_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_team_member tm
        USING rb_team t
        WHERE t.id = tm.team_id AND t.game_id = $1 AND t.tstate = 0
            AND tm.user_id = $2 AND tm.is_captain = FALSE",
        game_id,
        user_id
    )
    .execute(db_pool)
    .await?;

    if result.rows_affected() > 0 {
        update_user_team_cache(kv_pool, user_id, game_id, None).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub async fn disband(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    game_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let info = sqlx::query!(
        "SELECT team_id, user_id FROM rb_team_member
        WHERE team_id = (
            SELECT tm.team_id FROM rb_team_member tm
            JOIN rb_team t ON t.id = tm.team_id
            WHERE t.game_id = $1 AND t.tstate = 0
                AND tm.user_id = $2 AND tm.is_captain
        ) FOR UPDATE;",
        game_id,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;

    if info.is_empty() {
        return Ok(false);
    }

    let result = sqlx::query!(
        "DELETE FROM rb_team
        WHERE id = $1;",
        info[0].team_id
    )
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() > 0 {
        tx.commit().await?;

        let members: Vec<i32> = info.iter().map(|x| x.user_id).collect();
        update_users_team_cache(kv_pool, &members, game_id, None).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Deserialize, Validate)]
pub struct UserUpdateData {
    #[validate(length(min = 1, max = 40))]
    pub tname: Option<String>,
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

    if let Some(tname) = &data.tname {
        if !first {
            qb.push(", ");
        }
        qb.push("tname = ").push_bind(tname);
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
        db::cache::invalidate_team_info(app, team_id).await?;
        Ok(true)
    } else {
        Ok(false)
    }
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

#[derive(Serialize)]
pub struct RbCurrencyShowData {
    id: i32,
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

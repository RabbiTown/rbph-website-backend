use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::{
    error::RbInternalError,
    model::user::{RbUser, RbUserRole},
};

pub async fn register_user(
    pool: &PgPool,
    email: &str,
    upass: &str,
) -> Result<i32, RbInternalError> {
    let uid = sqlx::query_scalar!(
        "INSERT INTO rb_user (email, upass)
        VALUES ($1, $2)
        RETURNING id;",
        email,
        bcrypt::hash(upass, bcrypt::DEFAULT_COST)?,
    )
    .fetch_one(pool)
    .await?;

    Ok(uid)
}

pub async fn register_user_upass_hashed(
    pool: &PgPool,
    email: &str,
    upass_hashed: &str,
) -> Result<i32, RbInternalError> {
    let uid = sqlx::query_scalar!(
        "INSERT INTO rb_user (email, upass)
        VALUES ($1, $2)
        RETURNING id;",
        email,
        upass_hashed,
    )
    .fetch_one(pool)
    .await?;

    Ok(uid)
}

pub async fn check_user_exists(pool: &PgPool, email: &str) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM rb_user WHERE email = $1);",
        email
    )
    .fetch_one(pool)
    .await?;
    Ok(result.unwrap_or(false))
}

pub async fn get_user_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<RbUser>, RbInternalError> {
    let ret = sqlx::query_as!(RbUser, "SELECT * FROM rb_user WHERE email = $1;", email)
        .fetch_one(pool)
        .await;

    match ret {
        Ok(user) => Ok(Some(user)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(err) => Err(RbInternalError::Sql(err)),
    }
}

#[derive(Deserialize, Serialize)]
struct PendingUser {
    email: String,
    upass: String,
}

pub async fn put_pending_user(
    pool: &deadpool_redis::Pool,
    email: &str,
    upass: &str,
) -> Result<String, RbInternalError> {
    let mut conn = pool.get().await?;

    let token = Uuid::new_v4().to_string();
    let upass_hashed = bcrypt::hash(upass, bcrypt::DEFAULT_COST)?;

    let user = PendingUser {
        email: email.to_string(),
        upass: upass_hashed,
    };

    conn.set_ex::<_, _, ()>(
        format!("pending_user:{}", token),
        serde_json::to_string(&user).unwrap(),
        15 * 60,
    )
    .await?;

    Ok(token)
}

pub async fn verify_pending_user(
    db_pool: &PgPool,
    kv_pool: &deadpool_redis::Pool,
    token: &str,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;

    let data: Option<String> = conn.get_del(format!("pending_user:{}", token)).await?;
    if data.is_none() {
        return Ok(None);
    }

    let user: PendingUser = serde_json::from_str(&data.unwrap())?;
    let uid = register_user_upass_hashed(db_pool, &user.email, &user.upass).await?;

    Ok(Some(uid))
}

// TODO : add redis cache
pub async fn get_user_role_by_id(
    pool: &PgPool,
    user_id: i32,
) -> Result<Option<RbUserRole>, RbInternalError> {
    let ret = sqlx::query_scalar!("SELECT urole FROM rb_user WHERE id = $1;", user_id)
        .fetch_one(pool)
        .await
        .map(RbUserRole::from);

    match ret {
        Ok(role) => Ok(Some(role)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(err) => Err(RbInternalError::Sql(err)),
    }
}

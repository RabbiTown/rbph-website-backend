use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    DbPool, KvPool,
    error::RbInternalError,
    model::user::{RbUser, RbUserRole},
};

pub async fn register(pool: &DbPool, email: &str, pass: &str) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_user (email, pass)
        VALUES ($1, $2)
        RETURNING id;",
        email,
        bcrypt::hash(pass, bcrypt::DEFAULT_COST)?,
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn register_pass_hashed(
    pool: &DbPool,
    email: &str,
    pass_hashed: &str,
) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_user (email, pass)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        RETURNING id;",
        email,
        pass_hashed,
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn check_exists(pool: &DbPool, email: &str) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM rb_user WHERE email = $1);",
        email
    )
    .fetch_one(pool)
    .await?;
    Ok(result.unwrap_or(false))
}

pub async fn get_by_email(pool: &DbPool, email: &str) -> Result<Option<RbUser>, RbInternalError> {
    let result = sqlx::query_as!(RbUser, "SELECT * FROM rb_user WHERE email = $1;", email)
        .fetch_optional(pool)
        .await?;

    Ok(result)
}

#[derive(Deserialize, Serialize)]
struct PendingUser {
    email: String,
    pass: String,
}

pub async fn put_pending(
    pool: &KvPool,
    email: &str,
    pass: &str,
) -> Result<String, RbInternalError> {
    let mut conn = pool.get().await?;

    let token = Uuid::new_v4().to_string();
    let pass_hashed = bcrypt::hash(pass, bcrypt::DEFAULT_COST)?;

    let user = PendingUser {
        email: email.to_string(),
        pass: pass_hashed,
    };

    conn.set_ex::<_, _, ()>(
        format!("pending_user:{}", token),
        serde_json::to_string(&user).unwrap(),
        15 * 60,
    )
    .await?;

    Ok(token)
}

pub async fn verify_pending(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    token: &str,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;

    let data: Option<String> = conn.get_del(format!("pending_user:{}", token)).await?;
    if data.is_none() {
        return Ok(None);
    }

    let user: PendingUser = serde_json::from_str(&data.unwrap())?;
    let result = register_pass_hashed(db_pool, &user.email, &user.pass).await?;

    Ok(Some(result))
}

// TODO : add redis cache
pub async fn get_role_by_id(
    pool: &DbPool,
    user_id: i32,
) -> Result<Option<RbUserRole>, RbInternalError> {
    let result = sqlx::query_scalar!("SELECT urole FROM rb_user WHERE id = $1;", user_id)
        .fetch_optional(pool)
        .await?
        .map(RbUserRole::from);

    Ok(result)
}

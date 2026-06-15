use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{DbPool, KvPool, error::RbInternalError, model::user::RbUserRole};

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
        ON CONFLICT (email) DO NOTHING
        RETURNING id;",
        email,
        pass_hashed,
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn exists(pool: &DbPool, email: &str) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM rb_user WHERE email = $1);",
        email
    )
    .fetch_one(pool)
    .await?;

    Ok(result.unwrap_or(false))
}

#[derive(Deserialize)]
pub struct UserVerifyData {
    pub id: i32,
    pub pass: String,
}

pub async fn get_verify_by_email(
    pool: &DbPool,
    email: &str,
) -> Result<Option<UserVerifyData>, RbInternalError> {
    let result = sqlx::query_as!(
        UserVerifyData,
        "SELECT id, pass FROM rb_user WHERE email = $1;",
        email
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

#[derive(Deserialize, Serialize)]
struct PendingUser {
    email: String,
    pass: String,
}

pub async fn pending_exists(pool: &KvPool, email: &str) -> Result<bool, RbInternalError> {
    let mut conn = pool.get().await?;

    let key = format!("pending_email:{email}");
    let exists: Option<String> = conn.get(&key).await?;
    Ok(exists.is_some())
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
        format!("pending_user:{token}"),
        serde_json::to_string(&user).unwrap(),
        15 * 60,
    )
    .await?;

    conn.set_ex::<_, _, ()>(format!("pending_email:{email}"), token.clone(), 15 * 60)
        .await?;

    Ok(token)
}

pub async fn verify_pending(
    db_pool: &DbPool,
    kv_pool: &KvPool,
    token: &str,
) -> Result<Option<i32>, RbInternalError> {
    let mut conn = kv_pool.get().await?;

    let data: Option<String> = conn.get_del(format!("pending_user:{token}")).await?;
    if data.is_none() {
        return Ok(None);
    }

    let user: PendingUser = serde_json::from_str(&data.unwrap())?;
    let result = register_pass_hashed(db_pool, &user.email, &user.pass).await?;

    let _: () = conn.del(format!("pending_email:{}", user.email)).await?;

    Ok(Some(result))
}

#[derive(Deserialize, Serialize)]
pub struct RbUserDisplayData {
    pub id: i32,
    pub email: String,
    pub urole: RbUserRole,
    pub nickname: String,
    pub bio: Option<String>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

pub async fn get_display_by_id(
    pool: &DbPool,
    user_id: i32,
) -> Result<RbUserDisplayData, RbInternalError> {
    let result = sqlx::query_as!(
        RbUserDisplayData,
        "SELECT id, email, urole, nickname, bio, ctime_at
        FROM rb_user WHERE id = $1;",
        user_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
}

pub async fn update_profile(
    pool: &DbPool,
    user_id: i32,
    nickname: &str,
    bio: Option<&str>,
) -> Result<RbUserDisplayData, RbInternalError> {
    let result = sqlx::query_as!(
        RbUserDisplayData,
        "UPDATE rb_user
        SET nickname = $2, bio = $3
        WHERE id = $1
        RETURNING id, email, urole, nickname, bio, ctime_at;",
        user_id,
        nickname,
        bio
    )
    .fetch_one(pool)
    .await?;

    Ok(result)
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

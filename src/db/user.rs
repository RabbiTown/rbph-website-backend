use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    DbPool, KvPool,
    error::RbInternalError,
    model::user::{AvatarProvider, RbUserRole},
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
    pub must_change_password: bool,
}

pub async fn get_verify_by_email(
    pool: &DbPool,
    email: &str,
) -> Result<Option<UserVerifyData>, RbInternalError> {
    let result = sqlx::query_as!(
        UserVerifyData,
        "SELECT id, pass, must_change_password FROM rb_user WHERE email = $1;",
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
    pub must_change_password: bool,
    pub avatar: String,
    pub avatar_provider: AvatarProvider,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

struct RbUserDisplayRow {
    id: i32,
    email: String,
    urole: RbUserRole,
    nickname: String,
    bio: Option<String>,
    must_change_password: bool,
    avatar_provider: i16,
    ctime_at: OffsetDateTime,
}

impl From<RbUserDisplayRow> for RbUserDisplayData {
    fn from(user: RbUserDisplayRow) -> Self {
        let avatar_provider = AvatarProvider::try_from(user.avatar_provider).unwrap_or_default();
        let avatar = crate::model::user::avatar_url(&user.email, avatar_provider);
        Self {
            id: user.id,
            email: user.email,
            urole: user.urole,
            nickname: user.nickname,
            bio: user.bio,
            must_change_password: user.must_change_password,
            avatar,
            avatar_provider,
            ctime_at: user.ctime_at,
        }
    }
}

pub async fn get_display_by_id(
    pool: &DbPool,
    user_id: i32,
) -> Result<RbUserDisplayData, RbInternalError> {
    let result = sqlx::query_as!(
        RbUserDisplayRow,
        "SELECT id, email, urole, nickname, bio, must_change_password, avatar_provider, ctime_at
        FROM rb_user WHERE id = $1;",
        user_id
    )
    .fetch_one(pool)
    .await?;

    Ok(result.into())
}

pub async fn update_profile(
    pool: &DbPool,
    user_id: i32,
    nickname: &str,
    bio: Option<&str>,
    avatar_provider: AvatarProvider,
) -> Result<RbUserDisplayData, RbInternalError> {
    let result = sqlx::query_as!(
        RbUserDisplayRow,
        "UPDATE rb_user
        SET nickname = $2, bio = $3, avatar_provider = $4
        WHERE id = $1
        RETURNING id, email, urole, nickname, bio, must_change_password, avatar_provider, ctime_at;",
        user_id,
        nickname,
        bio,
        i16::from(avatar_provider)
    )
    .fetch_one(pool)
    .await?;

    Ok(result.into())
}

pub async fn team_ids(pool: &DbPool, user_id: i32) -> Result<Vec<i32>, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "SELECT team_id FROM rb_team_member WHERE user_id = $1;",
        user_id
    )
    .fetch_all(pool)
    .await?)
}

pub enum ChangePasswordResult {
    WrongCurrent,
    SamePassword,
    Ok,
}

pub async fn change_password(
    pool: &DbPool,
    user_id: i32,
    current_password: Option<&str>,
    new_password: &str,
) -> Result<ChangePasswordResult, RbInternalError> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query!(
        "SELECT pass, must_change_password FROM rb_user WHERE id = $1 FOR UPDATE;",
        user_id
    )
    .fetch_one(&mut *tx)
    .await?;

    if !current.must_change_password {
        let Some(current_password) = current_password else {
            tx.rollback().await?;
            return Ok(ChangePasswordResult::WrongCurrent);
        };
        if !bcrypt::verify(current_password, &current.pass)? {
            tx.rollback().await?;
            return Ok(ChangePasswordResult::WrongCurrent);
        }
    }
    if bcrypt::verify(new_password, &current.pass)? {
        tx.rollback().await?;
        return Ok(ChangePasswordResult::SamePassword);
    }

    let new_hash = bcrypt::hash(new_password, bcrypt::DEFAULT_COST)?;
    sqlx::query!(
        "UPDATE rb_user SET pass = $2, must_change_password = FALSE WHERE id = $1;",
        user_id,
        new_hash
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ChangePasswordResult::Ok)
}

#[derive(Clone, Copy)]
pub struct UserAuthState {
    pub role: RbUserRole,
    pub must_change_password: bool,
}

// TODO : add redis cache
pub async fn get_auth_state_by_id(
    pool: &DbPool,
    user_id: i32,
) -> Result<Option<UserAuthState>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT urole, must_change_password FROM rb_user WHERE id = $1;",
        user_id
    )
    .fetch_optional(pool)
    .await?
    .map(|row| UserAuthState {
        role: RbUserRole::from(row.urole),
        must_change_password: row.must_change_password,
    });

    Ok(result)
}

#[derive(Serialize)]
pub struct AdminUserListItem {
    pub id: i32,
    pub email: String,
    pub nickname: String,
    pub avatar: String,
    pub urole: RbUserRole,
    pub must_change_password: bool,
    pub team_count: i64,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

struct AdminUserListRow {
    id: i32,
    email: String,
    nickname: String,
    avatar_provider: i16,
    urole: i16,
    must_change_password: bool,
    team_count: i64,
    ctime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct AdminUserTeam {
    pub game_id: i32,
    pub game_title: String,
    pub team_id: i32,
    pub team_name: String,
    pub is_captain: bool,
}

#[derive(Serialize)]
pub struct AdminUserDetail {
    pub id: i32,
    pub email: String,
    pub nickname: String,
    pub bio: Option<String>,
    pub avatar: String,
    pub avatar_provider: AvatarProvider,
    pub urole: RbUserRole,
    pub must_change_password: bool,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
    pub teams: Vec<AdminUserTeam>,
}

#[derive(Clone, Copy)]
pub struct AdminUserListFilter<'a> {
    pub search: &'a str,
    pub role: Option<i16>,
    pub limit: i64,
    pub offset: i64,
}

pub async fn admin_list(
    pool: &DbPool,
    filter: AdminUserListFilter<'_>,
) -> Result<Vec<AdminUserListItem>, RbInternalError> {
    Ok(sqlx::query_as!(
        AdminUserListRow,
        r#"SELECT u.id, u.email, u.nickname, u.avatar_provider, u.urole,
            u.must_change_password, u.ctime_at, COUNT(tm.team_id) AS "team_count!"
        FROM rb_user u
        LEFT JOIN rb_team_member tm ON tm.user_id = u.id
        WHERE ($1 = '' OR u.email ILIKE '%' || $1 || '%'
            OR u.nickname ILIKE '%' || $1 || '%'
            OR u.id::TEXT = $1)
            AND ($2::SMALLINT IS NULL OR u.urole = $2)
        GROUP BY u.id
        ORDER BY u.id
        LIMIT $3 OFFSET $4"#,
        filter.search,
        filter.role,
        filter.limit,
        filter.offset,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| AdminUserListItem {
        id: row.id,
        avatar: crate::model::user::avatar_url(
            &row.email,
            AvatarProvider::try_from(row.avatar_provider).unwrap_or_default(),
        ),
        email: row.email,
        nickname: row.nickname,
        urole: RbUserRole::from(row.urole),
        must_change_password: row.must_change_password,
        team_count: row.team_count,
        ctime_at: row.ctime_at,
    })
    .collect())
}

pub async fn admin_count(
    pool: &DbPool,
    filter: AdminUserListFilter<'_>,
) -> Result<i64, RbInternalError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM rb_user u
        WHERE ($1 = '' OR u.email ILIKE '%' || $1 || '%'
            OR u.nickname ILIKE '%' || $1 || '%'
            OR u.id::TEXT = $1)
            AND ($2::SMALLINT IS NULL OR u.urole = $2)"#,
        filter.search,
        filter.role,
    )
    .fetch_one(pool)
    .await?)
}

pub async fn admin_get(
    pool: &DbPool,
    user_id: i32,
) -> Result<Option<AdminUserDetail>, RbInternalError> {
    let user = sqlx::query!(
        "SELECT id, email, nickname, bio, avatar_provider, urole, must_change_password, ctime_at
        FROM rb_user WHERE id = $1",
        user_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(user) = user else { return Ok(None) };
    let teams = sqlx::query_as!(
        AdminUserTeam,
        "SELECT g.id AS game_id, g.title AS game_title, t.id AS team_id,
            t.name AS team_name, tm.is_captain
        FROM rb_team_member tm
        JOIN rb_team t ON t.id = tm.team_id
        JOIN rb_game g ON g.id = tm.game_id
        WHERE tm.user_id = $1
        ORDER BY g.id, t.id",
        user_id
    )
    .fetch_all(pool)
    .await?;
    let avatar_provider = AvatarProvider::try_from(user.avatar_provider).unwrap_or_default();
    Ok(Some(AdminUserDetail {
        id: user.id,
        avatar: crate::model::user::avatar_url(&user.email, avatar_provider),
        avatar_provider,
        email: user.email,
        nickname: user.nickname,
        bio: user.bio,
        urole: RbUserRole::from(user.urole),
        must_change_password: user.must_change_password,
        ctime_at: user.ctime_at,
        teams,
    }))
}

pub async fn admin_create(
    pool: &DbPool,
    email: &str,
    nickname: &str,
    bio: Option<&str>,
    role: RbUserRole,
    password: &str,
) -> Result<Option<i32>, RbInternalError> {
    let pass = bcrypt::hash(password, bcrypt::DEFAULT_COST)?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO rb_user (email, pass, nickname, bio, urole, must_change_password)
        VALUES ($1, $2, $3, $4, $5, TRUE)
        ON CONFLICT (email) DO NOTHING RETURNING id",
        email,
        pass,
        nickname,
        bio,
        i16::from(role),
    )
    .fetch_optional(pool)
    .await?)
}

pub enum AdminUserUpdateResult {
    Ok,
    NotFound,
    EmailConflict,
    SelfRole,
    RoleForbidden,
}

pub struct AdminUserUpdateData<'a> {
    pub email: &'a str,
    pub nickname: &'a str,
    pub bio: Option<&'a str>,
    pub role: RbUserRole,
}

pub async fn admin_update(
    pool: &DbPool,
    actor_id: i32,
    actor_role: RbUserRole,
    user_id: i32,
    data: AdminUserUpdateData<'_>,
) -> Result<AdminUserUpdateResult, RbInternalError> {
    let mut tx = pool.begin().await?;
    let current_role = sqlx::query_scalar!(
        "SELECT urole FROM rb_user WHERE id = $1 FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current_role) = current_role else {
        return Ok(AdminUserUpdateResult::NotFound);
    };
    let current_role = RbUserRole::from(current_role);
    if actor_id == user_id && current_role != data.role {
        return Ok(AdminUserUpdateResult::SelfRole);
    }
    if !actor_role.can_change_role(Some(current_role), data.role) {
        return Ok(AdminUserUpdateResult::RoleForbidden);
    }
    let updated = sqlx::query_scalar!(
        "UPDATE rb_user SET email = $2, nickname = $3, bio = $4, urole = $5
        WHERE id = $1 AND NOT EXISTS (
            SELECT 1 FROM rb_user other WHERE other.email = $2 AND other.id <> $1
        ) RETURNING id",
        user_id,
        data.email,
        data.nickname,
        data.bio,
        i16::from(data.role),
    )
    .fetch_optional(&mut *tx)
    .await?;
    if updated.is_none() {
        return Ok(AdminUserUpdateResult::EmailConflict);
    }
    tx.commit().await?;
    Ok(AdminUserUpdateResult::Ok)
}

pub enum AdminUserResetPasswordResult {
    Ok,
    NotFound,
    RoleForbidden,
}

pub async fn admin_reset_password(
    pool: &DbPool,
    actor_role: RbUserRole,
    user_id: i32,
    password: &str,
) -> Result<AdminUserResetPasswordResult, RbInternalError> {
    let mut tx = pool.begin().await?;
    let target_role = sqlx::query_scalar!(
        "SELECT urole FROM rb_user WHERE id = $1 FOR UPDATE",
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(target_role) = target_role else {
        return Ok(AdminUserResetPasswordResult::NotFound);
    };
    if !actor_role.can_manage_credentials(RbUserRole::from(target_role)) {
        return Ok(AdminUserResetPasswordResult::RoleForbidden);
    }

    let pass = bcrypt::hash(password, bcrypt::DEFAULT_COST)?;
    sqlx::query!(
        "UPDATE rb_user SET pass = $2, must_change_password = TRUE
        WHERE id = $1",
        user_id,
        pass,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(AdminUserResetPasswordResult::Ok)
}

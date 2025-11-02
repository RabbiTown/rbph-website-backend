use crate::{api::RbError, config::Settings, db};
use actix_session::Session;
use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use sqlx::PgPool;

static EMAIL_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\w+(-+.\w+)*@\w+(-.\w+)*.\w+(-.\w+)*$").unwrap());

static PWD_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[!-~]{8,64}$").unwrap());

#[derive(Serialize)]
struct UserPreLoginResponse {}

async fn pre_auth(cfg: web::Data<Settings>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(UserPreLoginResponse {}))
}

#[derive(Deserialize)]
struct UserLoginRequest {
    email: String,
    password: String,
    captcha: Option<String>,
}

#[derive(Serialize)]
struct UserLoginResponse {
    code: UserLoginResult,
    uid: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserLoginResult {
    BCryptError = -10,
    NotExists = -2,
    WrongPwd = -1,
    Ok = 0,
}

async fn login(
    req: web::Json<UserLoginRequest>,
    sess: Session,
    pool: web::Data<PgPool>,
) -> Result<HttpResponse> {
    let trimmed_email = req.email.trim();
    if !EMAIL_REGEX.is_match(trimmed_email) {
        RbError::with_code(UserLoginResult::WrongPwd.into()).err()?
    }

    let trimmed_pwd = req.password.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::with_code(UserLoginResult::WrongPwd.into()).err()?
    }

    let user = db::user::get_user_by_email(&pool, &req.email).await?;
    if user.is_none() {
        RbError::with_code(UserLoginResult::NotExists.into()).err()?
    }

    let user = user.unwrap();
    match bcrypt::verify(&req.password, &user.upass) {
        Ok(true) => {}
        Ok(false) => RbError::with_code(UserLoginResult::NotExists.into()).err()?,
        Err(e) => RbError::with_code(UserLoginResult::BCryptError.into()).intern_err(e)?,
    }

    sess.insert("user_id", user.id)?;

    Ok(HttpResponse::Ok().json(UserLoginResponse {
        code: UserLoginResult::Ok,
        uid: user.id,
    }))
}

#[derive(Deserialize)]
struct UserRegisterRequest {
    email: String,
    password: String,
    captcha: Option<String>,
}

#[derive(Serialize)]
struct UserRegisterResponse {
    code: UserRegisterResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    uid: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserRegisterResult {
    InvalidPassword = -3,
    InvalidEmail = -2,
    UserExists = -1,
    Ok = 0,
    EmailSent = 1,
}

async fn register(
    req: web::Json<UserRegisterRequest>,
    db_pool: web::Data<PgPool>,
    kv_pool: web::Data<deadpool_redis::Pool>,
) -> Result<HttpResponse> {
    let trimmed_email = req.email.trim();
    if !EMAIL_REGEX.is_match(trimmed_email) {
        RbError::with_code(UserRegisterResult::InvalidEmail.into()).err()?
    }

    let trimmed_pwd = req.password.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::with_code(UserRegisterResult::InvalidPassword.into()).err()?
    }

    if db::user::check_user_exists(&db_pool, &req.email).await? {
        RbError::with_code(UserRegisterResult::UserExists.into()).err()?
    }

    let token = db::user::put_pending_user(&kv_pool, trimmed_email, trimmed_pwd).await?;

    log::debug!("register : {} ({})", trimmed_email, token);

    // TODO : email configuration

    Ok(HttpResponse::Ok().json(UserRegisterResponse {
        code: UserRegisterResult::EmailSent,
        uid: None,
    }))
}

#[derive(Deserialize)]
pub struct UserVerifyQuery {
    token: String,
}

#[derive(Serialize)]
struct UserVerifyResponse {
    code: UserVerifyResult,
    uid: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserVerifyResult {
    Invalid = -1,
    Ok = 0,
}

async fn verify(
    req: web::Query<UserVerifyQuery>,
    db_pool: web::Data<PgPool>,
    kv_pool: web::Data<deadpool_redis::Pool>,
) -> Result<HttpResponse> {
    let result = db::user::verify_pending_user(&db_pool, &kv_pool, &req.token).await?;
    if result.is_none() {
        RbError::with_code(UserVerifyResult::Invalid.into()).err()?
    }
    Ok(HttpResponse::Ok().json(UserVerifyResponse {
        code: UserVerifyResult::Ok,
        uid: result.unwrap(),
    }))
}

#[derive(Serialize)]
struct UserLogoutResponse {
    code: UserLogoutResult,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserLogoutResult {
    Ok = 0,
}

async fn logout(sess: Session) -> Result<HttpResponse> {
    sess.purge();

    Ok(HttpResponse::Ok().json(UserLogoutResponse {
        code: UserLogoutResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("pre-auth", web::get().to(pre_auth))
        .route("login", web::post().to(login))
        .route("register", web::post().to(register))
        .route("verify", web::get().to(verify))
        .route("logout", web::post().to(logout));
}

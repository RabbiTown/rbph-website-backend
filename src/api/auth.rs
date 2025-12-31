use crate::{AppState, db, error::RbError, module::session};
use actix_session::Session;
use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

static EMAIL_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\w+(-+.\w+)*@\w+(-.\w+)*.\w+(-.\w+)*$").unwrap());

static PWD_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[!-~]{8,64}$").unwrap());

#[derive(Serialize)]
struct UserPreLoginResponse {}

async fn pre_auth(_app: web::Data<AppState>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(UserPreLoginResponse {}))
}

#[derive(Deserialize)]
struct UserLoginRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct UserLoginResponse {
    code: UserLoginResult,
    uid: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserLoginResult {
    NotExists = -2,
    WrongPwd = -1,
    Ok = 0,
}

async fn login(
    req: web::Json<UserLoginRequest>,
    sess: Session,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let trimmed_email = req.email.trim().to_lowercase();
    if !EMAIL_REGEX.is_match(&trimmed_email) {
        RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .err()?
    }

    let trimmed_pwd = req.password.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .err()?
    }

    let user = db::user::get_verify_by_email(&app.db, &trimmed_email).await?;
    if user.is_none() {
        RbError::unauth()
            .code(UserLoginResult::NotExists.into())
            .err()?
    }

    let user = user.unwrap();
    match bcrypt::verify(trimmed_pwd, &user.pass) {
        Ok(true) => {}
        Ok(false) => RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .err()?,
        Err(e) => RbError::internal(e).err()?,
    }

    session::append(&app.kv, &sess, user.id, app.settings.auth.max_session).await?;
    sess.renew();

    Ok(HttpResponse::Ok().json(UserLoginResponse {
        code: UserLoginResult::Ok,
        uid: user.id,
    }))
}

#[derive(Deserialize)]
struct UserRegisterRequest {
    email: String,
    password: String,
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
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let trimmed_email = req.email.trim().to_lowercase();
    if !EMAIL_REGEX.is_match(&trimmed_email) {
        RbError::bad_req(UserRegisterResult::InvalidEmail.into()).err()?
    }

    let trimmed_pwd = req.password.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::bad_req(UserRegisterResult::InvalidPassword.into()).err()?
    }

    if db::user::exists(&app.db, &trimmed_email).await? {
        RbError::conflict(UserRegisterResult::UserExists.into()).err()?
    }

    let token = db::user::put_pending(&app.kv, &trimmed_email, trimmed_pwd).await?;

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
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::user::verify_pending(&app.db, &app.kv, &req.token).await?;
    if result.is_none() {
        RbError::bad_req(UserVerifyResult::Invalid.into()).err()?
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

async fn logout(sess: Session, app: web::Data<AppState>) -> Result<HttpResponse> {
    session::invalidate(&app.kv, &sess).await?;
    sess.purge();

    Ok(HttpResponse::Ok().json(UserLogoutResponse {
        code: UserLogoutResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/pre-auth", web::get().to(pre_auth))
        .route("/login", web::post().to(login))
        .route("/register", web::post().to(register))
        .route("/verify", web::get().to(verify))
        .route("/logout", web::post().to(logout));
}

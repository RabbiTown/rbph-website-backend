use crate::{AppState, db, error::RbError, module::session};
use actix_session::Session;
use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use validator::{Validate, ValidationError};

fn validate_printable(s: &str) -> Result<(), ValidationError> {
    if !s.is_ascii() {
        return Err(ValidationError::new("ascii"));
    }

    if !s.bytes().all(|b| (b'!'..=b'~').contains(&b)) {
        return Err(ValidationError::new("printable_ascii"));
    }

    Ok(())
}

// -- pre-login --

#[derive(Serialize)]
struct UserPreLoginResponse {}

async fn pre_auth(_app: web::Data<AppState>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(UserPreLoginResponse {}))
}

// -- login --

#[derive(Deserialize, Validate)]
struct UserLoginRequest {
    #[validate(email)]
    email: String,
    #[validate(custom(function = validate_printable), length(min = 8, max = 64))]
    password: String,
}

impl UserLoginRequest {
    fn normalized(&self) -> Self {
        Self {
            email: self.email.trim().to_lowercase(),
            password: self.password.trim().to_string(),
        }
    }
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
    let req = req.normalized();
    if let Err(e) = req.validate() {
        RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .msg(e.to_string())
            .err()?;
    }

    let user = db::user::get_verify_by_email(&app.db, &req.email).await?;
    if user.is_none() {
        RbError::unauth()
            .code(UserLoginResult::NotExists.into())
            .err()?
    }

    let user = user.unwrap();
    match bcrypt::verify(&req.password, &user.pass) {
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

// -- register --

#[derive(Deserialize, Validate)]
struct UserRegisterRequest {
    #[validate(email)]
    email: String,
    #[validate(custom(function = validate_printable), length(min = 8, max = 64))]
    password: String,
}

impl UserRegisterRequest {
    fn normalized(&self) -> Self {
        Self {
            email: self.email.trim().to_lowercase(),
            password: self.password.trim().to_string(),
        }
    }
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
    Invalid = -2,
    UserExists = -1,
    Ok = 0,
    EmailSent = 1,
    EmailAlreadySent = 2,
}

async fn register(
    req: web::Json<UserRegisterRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let req = req.normalized();
    if let Err(e) = req.validate() {
        RbError::bad_req(UserRegisterResult::Invalid.into())
            .msg(e.to_string())
            .err()?;
    }

    if db::user::exists(&app.db, &req.email).await? {
        RbError::conflict(UserRegisterResult::UserExists.into()).err()?
    }

    if app.settings.auth.email.enabled
        && let Some(email) = &app.email
    {
        if db::user::pending_exists(&app.kv, &req.email).await? {
            return Ok(HttpResponse::Ok().json(UserRegisterResponse {
                code: UserRegisterResult::EmailAlreadySent,
                uid: None,
            }));
        }

        let token = db::user::put_pending(&app.kv, &req.email, &req.password).await?;

        log::debug!("register : {} ({})", req.email, token);

        email
            .send_verify_email(
                &req.email,
                &app.settings.auth.email.url.verify.replace("{}", &token),
            )
            .await?;

        Ok(HttpResponse::Ok().json(UserRegisterResponse {
            code: UserRegisterResult::EmailSent,
            uid: None,
        }))
    } else {
        let uid = db::user::register(&app.db, &req.email, &req.password).await?;
        Ok(HttpResponse::Ok().json(UserRegisterResponse {
            code: UserRegisterResult::Ok,
            uid: Some(uid),
        }))
    }
}

// -- verify --

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

// -- logout --

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

use crate::{
    AppState, db,
    error::{RbError, RbInternalError},
    module::{
        auth_rate_limit,
        captcha::{CaptchaAction, CaptchaPublicConfig, CaptchaVerifyError},
        session,
    },
};
use actix_session::Session;
use actix_web::{HttpRequest, HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::json;
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

const DUMMY_PASSWORD_HASH: &str = "$2b$12$tu7u2NM5PFaFcs3F.ZykLe8F2olKRQYH8zSQK9hybJdDZta8Pmnd6";

fn client_identifier(req: &HttpRequest) -> String {
    let connection_info = req.connection_info();
    connection_info
        .realip_remote_addr()
        .map(str::to_owned)
        .or_else(|| req.peer_addr().map(|address| address.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

async fn consume_rate_limit(
    app: &AppState,
    key: &str,
    limit: u64,
    window_seconds: u64,
) -> Result<(), actix_web::Error> {
    if let Some(retry_after) = auth_rate_limit::consume(&app.kv, key, limit, window_seconds)
        .await
        .map_err(rate_limit_unavailable)?
    {
        return Err(RbError::too_many_requests(retry_after).into());
    }
    Ok(())
}

fn rate_limit_unavailable(error: RbInternalError) -> actix_web::Error {
    log::warn!("Authentication rate limiter unavailable: {error:?}");
    RbError::service_unavailable().into()
}

// -- pre-login --

#[derive(Serialize)]
struct CaptchaPreAuthResponse {
    #[serde(flatten)]
    config: CaptchaPublicConfig,
    login_required: bool,
    registration_required: bool,
}

#[derive(Serialize)]
struct UserPreLoginResponse {
    code: i32,
    captcha: Option<CaptchaPreAuthResponse>,
}

async fn pre_auth(app: web::Data<AppState>) -> Result<HttpResponse> {
    let settings = app.system_settings.read().await;
    let captcha = app.captcha.as_ref().map(|service| CaptchaPreAuthResponse {
        config: service.public_config(),
        login_required: settings.captcha_login_required,
        registration_required: settings.captcha_registration_required,
    });
    Ok(HttpResponse::Ok().json(UserPreLoginResponse { code: 0, captcha }))
}

async fn verify_captcha(
    app: &AppState,
    required: bool,
    token: Option<&str>,
    action: CaptchaAction,
) -> Result<(), actix_web::Error> {
    if !required {
        return Ok(());
    }
    let Some(captcha) = &app.captcha else {
        return Err(RbError::captcha_unavailable().into());
    };
    match captcha.verify(token, action).await {
        Ok(()) => Ok(()),
        Err(CaptchaVerifyError::Invalid) => Err(RbError::captcha_invalid().into()),
        Err(CaptchaVerifyError::Unavailable) => Err(RbError::captcha_unavailable().into()),
    }
}

// -- login --

#[derive(Deserialize, Validate)]
struct UserLoginRequest {
    #[validate(email)]
    email: String,
    #[validate(custom(function = validate_printable), length(min = 8, max = 64))]
    password: String,
    captcha_token: Option<String>,
}

impl UserLoginRequest {
    fn normalized(&self) -> Self {
        Self {
            email: self.email.trim().to_lowercase(),
            password: self.password.trim().to_string(),
            captcha_token: self.captcha_token.clone(),
        }
    }
}

#[derive(Serialize)]
struct UserLoginResponse {
    code: UserLoginResult,
    uid: i32,
    must_change_password: bool,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserLoginResult {
    WrongPwd = -1,
    Ok = 0,
}

async fn login(
    req: web::Json<UserLoginRequest>,
    sess: Session,
    http_req: HttpRequest,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let req = req.normalized();
    let rate_limit = &app.settings.auth.rate_limit;
    let client = client_identifier(&http_req);
    let login_ip_email_key = rate_limit
        .enabled
        .then(|| auth_rate_limit::key("login_ip_email", &format!("{client}\0{}", req.email)));

    if rate_limit.enabled {
        consume_rate_limit(
            app.get_ref(),
            &auth_rate_limit::key("login_ip", &client),
            rate_limit.login_ip_attempts,
            rate_limit.login_window_seconds,
        )
        .await?;

        if let Some(retry_after) = auth_rate_limit::blocked(
            &app.kv,
            login_ip_email_key.as_deref().unwrap(),
            rate_limit.login_ip_email_failures,
        )
        .await
        .map_err(rate_limit_unavailable)?
        {
            return RbError::too_many_requests(retry_after).http_err();
        }
    }

    if req.validate().is_err() {
        if let Some(key) = &login_ip_email_key {
            consume_rate_limit(
                app.get_ref(),
                key,
                rate_limit.login_ip_email_failures,
                rate_limit.login_window_seconds,
            )
            .await?;
        }
        RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .err()?;
    }
    let captcha_required = app.system_settings.read().await.captcha_login_required;
    verify_captcha(
        app.get_ref(),
        captcha_required,
        req.captcha_token.as_deref(),
        CaptchaAction::Login,
    )
    .await?;

    let user = db::user::get_verify_by_email(&app.db, &req.email).await?;
    let password_valid = match &user {
        Some(user) => bcrypt::verify(&req.password, &user.pass),
        None => bcrypt::verify(&req.password, DUMMY_PASSWORD_HASH).map(|_| false),
    }
    .map_err(RbError::internal)?;

    if !password_valid {
        if let Some(key) = &login_ip_email_key {
            consume_rate_limit(
                app.get_ref(),
                key,
                rate_limit.login_ip_email_failures,
                rate_limit.login_window_seconds,
            )
            .await?;
        }
        return RbError::unauth()
            .code(UserLoginResult::WrongPwd.into())
            .http_err();
    }

    if let Some(key) = &login_ip_email_key {
        auth_rate_limit::clear(&app.kv, key)
            .await
            .map_err(rate_limit_unavailable)?;
    }
    let Some(user) = user else {
        return RbError::internal("password verification state is inconsistent").http_err();
    };

    let max_sessions = app.system_settings.read().await.max_sessions as usize;
    session::append(&app.kv, &sess, user.id, max_sessions).await?;
    sess.renew();

    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "auth.login",
            event_scope: i16::from(db::event_log::EventScope::Security),
            severity: i16::from(db::event_log::EventSeverity::Info),
            user_id: Some(user.id),
            data: json!({}),
            ..Default::default()
        },
    )
    .await?;

    Ok(HttpResponse::Ok().json(UserLoginResponse {
        code: UserLoginResult::Ok,
        uid: user.id,
        must_change_password: user.must_change_password,
    }))
}

// -- register --

#[derive(Deserialize, Validate)]
struct UserRegisterRequest {
    #[validate(email)]
    email: String,
    #[validate(custom(function = validate_printable), length(min = 8, max = 64))]
    password: String,
    captcha_token: Option<String>,
}

impl UserRegisterRequest {
    fn normalized(&self) -> Self {
        Self {
            email: self.email.trim().to_lowercase(),
            password: self.password.trim().to_string(),
            captcha_token: self.captcha_token.clone(),
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
    RegistrationClosed = -3,
    Invalid = -2,
    UserExists = -1,
    Ok = 0,
    EmailSent = 1,
    EmailAlreadySent = 2,
}

async fn register(
    req: web::Json<UserRegisterRequest>,
    http_req: HttpRequest,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let settings = app.system_settings.read().await.clone();
    if !settings.registration_open {
        return RbError::forbid()
            .code(UserRegisterResult::RegistrationClosed.into())
            .http_err();
    }

    let req = req.normalized();
    let rate_limit = &app.settings.auth.rate_limit;
    if rate_limit.enabled {
        consume_rate_limit(
            app.get_ref(),
            &auth_rate_limit::key("registration_ip", &client_identifier(&http_req)),
            rate_limit.registration_ip_attempts,
            rate_limit.registration_window_seconds,
        )
        .await?;
        consume_rate_limit(
            app.get_ref(),
            &auth_rate_limit::key("registration_email", &req.email),
            rate_limit.registration_email_attempts,
            rate_limit.registration_window_seconds,
        )
        .await?;
    }

    if let Err(e) = req.validate() {
        RbError::bad_req(UserRegisterResult::Invalid.into())
            .msg(e.to_string())
            .err()?;
    }
    verify_captcha(
        app.get_ref(),
        settings.captcha_registration_required,
        req.captcha_token.as_deref(),
        CaptchaAction::Register,
    )
    .await?;

    if db::user::exists(&app.db, &req.email).await? {
        RbError::conflict(UserRegisterResult::UserExists.into()).err()?
    }

    if settings.require_email_verification
        && let Some(email) = &app.email
    {
        if db::user::pending_exists(&app.kv, &req.email).await? {
            return Ok(HttpResponse::Ok().json(UserRegisterResponse {
                code: UserRegisterResult::EmailAlreadySent,
                uid: None,
            }));
        }

        let token = db::user::put_pending(&app.kv, &req.email, &req.password).await?;

        email
            .send_verify_email(
                &req.email,
                &app.settings.auth.email.url.verify.replace("{}", &token),
            )
            .await?;

        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "auth.register_requested",
                event_scope: i16::from(db::event_log::EventScope::Security),
                severity: i16::from(db::event_log::EventSeverity::Info),
                data: json!({ "email": req.email }),
                ..Default::default()
            },
        )
        .await?;

        Ok(HttpResponse::Ok().json(UserRegisterResponse {
            code: UserRegisterResult::EmailSent,
            uid: None,
        }))
    } else {
        let uid = db::user::register(&app.db, &req.email, &req.password).await?;
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "auth.registered",
                event_scope: i16::from(db::event_log::EventScope::Security),
                severity: i16::from(db::event_log::EventSeverity::Info),
                user_id: Some(uid),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;
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
    let uid = result.unwrap();
    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "auth.verified",
            event_scope: i16::from(db::event_log::EventScope::Security),
            severity: i16::from(db::event_log::EventSeverity::Info),
            user_id: Some(uid),
            data: json!({}),
            ..Default::default()
        },
    )
    .await?;
    Ok(HttpResponse::Ok().json(UserVerifyResponse {
        code: UserVerifyResult::Ok,
        uid,
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
    let user_id = sess.get::<i32>("user_id").ok().flatten();
    session::invalidate(&app.kv, &sess).await?;
    sess.purge();

    if let Some(user_id) = user_id {
        db::event_log::insert_pool(
            &app.db,
            db::event_log::EventLogInput {
                event_type: "auth.logout",
                event_scope: i16::from(db::event_log::EventScope::Security),
                severity: i16::from(db::event_log::EventSeverity::Info),
                user_id: Some(user_id),
                data: json!({}),
                ..Default::default()
            },
        )
        .await?;
    }

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

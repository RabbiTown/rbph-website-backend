use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_repr::Serialize_repr;
use validator::Validate;

use crate::{
    AppState, db, error::RbError, extractor::auth::AuthUser, model::user::RbUserRole,
    module::session,
};

#[derive(Deserialize)]
struct UserPath {
    user_id: i32,
}

#[derive(Deserialize)]
struct UserListQuery {
    search: Option<String>,
    role: Option<i16>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize, Validate)]
struct UserWriteRequest {
    #[validate(email, length(max = 255))]
    email: String,
    #[validate(length(min = 1, max = 60))]
    nickname: String,
    #[validate(length(max = 200))]
    bio: Option<String>,
    role: i16,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum UserAdminResult {
    RoleForbidden = -6,
    SelfRole = -4,
    Conflict = -3,
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct UserListResponse {
    code: UserAdminResult,
    users: Vec<db::user::AdminUserListItem>,
    total: i64,
}

#[derive(Serialize)]
struct UserResponse {
    code: UserAdminResult,
    user: db::user::AdminUserDetail,
}

#[derive(Serialize)]
struct TemporaryPasswordResponse {
    code: UserAdminResult,
    user: db::user::AdminUserDetail,
    temporary_password: String,
}

fn normalize(mut request: UserWriteRequest) -> UserWriteRequest {
    request.email = request.email.trim().to_lowercase();
    request.nickname = request.nickname.trim().to_string();
    request.bio = request
        .bio
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    request
}

fn validate_request(request: &UserWriteRequest) -> bool {
    request.validate().is_ok()
        && RbUserRole::from(request.role).is_valid()
        && !request.nickname.is_empty()
        && request
            .bio
            .as_ref()
            .is_none_or(|value| value.chars().count() <= 200)
}

fn temporary_password() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%";
    let mut rng = rand::rng();
    (0..16)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

async fn list(query: web::Query<UserListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    if query
        .role
        .is_some_and(|role| !RbUserRole::from(role).is_valid())
    {
        return RbError::bad_req(UserAdminResult::Invalid.into()).http_err();
    }
    let filter = db::user::AdminUserListFilter {
        search: query.search.as_deref().unwrap_or("").trim(),
        role: query.role,
        limit: query.limit.unwrap_or(20).clamp(1, 100),
        offset: query.offset.unwrap_or(0).max(0),
    };
    let users = db::user::admin_list(&app.db, filter).await?;
    let total = db::user::admin_count(&app.db, filter).await?;
    Ok(HttpResponse::Ok().json(UserListResponse {
        code: UserAdminResult::Ok,
        users,
        total,
    }))
}

async fn get(path: web::Path<UserPath>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let Some(user) = db::user::admin_get(&app.db, path.user_id).await? else {
        return RbError::not_found()
            .code(UserAdminResult::NotFound.into())
            .http_err();
    };
    Ok(HttpResponse::Ok().json(UserResponse {
        code: UserAdminResult::Ok,
        user,
    }))
}

async fn create(
    request: web::Json<UserWriteRequest>,
    actor: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let request = normalize(request.into_inner());
    if !validate_request(&request) {
        return RbError::bad_req(UserAdminResult::Invalid.into()).http_err();
    }
    let password = temporary_password();
    let role = RbUserRole::from(request.role);
    if !actor.req_role()?.can_change_role(None, role) {
        return RbError::forbid()
            .code(UserAdminResult::RoleForbidden.into())
            .http_err();
    }
    let Some(user_id) = db::user::admin_create(
        &app.db,
        &request.email,
        &request.nickname,
        request.bio.as_deref(),
        role,
        &password,
    )
    .await?
    else {
        return RbError::conflict(UserAdminResult::Conflict.into()).http_err();
    };
    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "admin.user_created",
            event_scope: i16::from(db::event_log::EventScope::Admin),
            severity: i16::from(db::event_log::EventSeverity::Info),
            user_id: Some(actor.uid),
            target_user_id: Some(user_id),
            data: json!({ "role": request.role }),
            ..Default::default()
        },
    )
    .await?;
    let user = db::user::admin_get(&app.db, user_id)
        .await?
        .ok_or(RbError::internal("Created user not found"))?;
    Ok(HttpResponse::Ok().json(TemporaryPasswordResponse {
        code: UserAdminResult::Ok,
        user,
        temporary_password: password,
    }))
}

async fn update(
    path: web::Path<UserPath>,
    request: web::Json<UserWriteRequest>,
    actor: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let request = normalize(request.into_inner());
    if !validate_request(&request) {
        return RbError::bad_req(UserAdminResult::Invalid.into()).http_err();
    }
    let role = RbUserRole::from(request.role);
    match db::user::admin_update(
        &app.db,
        actor.uid,
        actor.req_role()?,
        path.user_id,
        db::user::AdminUserUpdateData {
            email: &request.email,
            nickname: &request.nickname,
            bio: request.bio.as_deref(),
            role,
        },
    )
    .await?
    {
        db::user::AdminUserUpdateResult::NotFound => {
            return RbError::not_found().code(-1).http_err();
        }
        db::user::AdminUserUpdateResult::EmailConflict => {
            return RbError::conflict(UserAdminResult::Conflict.into()).http_err();
        }
        db::user::AdminUserUpdateResult::SelfRole => {
            return RbError::conflict(UserAdminResult::SelfRole.into()).http_err();
        }
        db::user::AdminUserUpdateResult::RoleForbidden => {
            return RbError::forbid()
                .code(UserAdminResult::RoleForbidden.into())
                .http_err();
        }
        db::user::AdminUserUpdateResult::Ok => {}
    }
    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "admin.user_updated",
            event_scope: i16::from(db::event_log::EventScope::Admin),
            severity: i16::from(db::event_log::EventSeverity::Info),
            user_id: Some(actor.uid),
            target_user_id: Some(path.user_id),
            data: json!({ "role": request.role }),
            ..Default::default()
        },
    )
    .await?;
    get(path, app).await
}

async fn reset_password(
    path: web::Path<UserPath>,
    actor: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let password = temporary_password();
    match db::user::admin_reset_password(&app.db, actor.req_role()?, path.user_id, &password)
        .await?
    {
        db::user::AdminUserResetPasswordResult::NotFound => {
            return RbError::not_found()
                .code(UserAdminResult::NotFound.into())
                .http_err();
        }
        db::user::AdminUserResetPasswordResult::RoleForbidden => {
            return RbError::forbid()
                .code(UserAdminResult::RoleForbidden.into())
                .http_err();
        }
        db::user::AdminUserResetPasswordResult::Ok => {}
    }
    session::invalidate_all(&app.kv, path.user_id).await?;
    db::event_log::insert_pool(
        &app.db,
        db::event_log::EventLogInput {
            event_type: "admin.user_password_reset",
            event_scope: i16::from(db::event_log::EventScope::Security),
            severity: i16::from(db::event_log::EventSeverity::Warning),
            user_id: Some(actor.uid),
            target_user_id: Some(path.user_id),
            data: json!({}),
            ..Default::default()
        },
    )
    .await?;
    let user = db::user::admin_get(&app.db, path.user_id)
        .await?
        .ok_or(RbError::internal("Reset user not found"))?;
    Ok(HttpResponse::Ok().json(TemporaryPasswordResponse {
        code: UserAdminResult::Ok,
        user,
        temporary_password: password,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/users", web::get().to(list))
        .route("/users", web::post().to(create))
        .route("/users/{user_id}", web::get().to(get))
        .route("/users/{user_id}", web::patch().to(update))
        .route(
            "/users/{user_id}/reset-password",
            web::post().to(reset_password),
        );
}

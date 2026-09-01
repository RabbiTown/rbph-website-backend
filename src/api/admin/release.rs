use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use time::OffsetDateTime;

use crate::{
    AppState,
    db::release::{
        self, RELEASE_VISIBILITY_HIDDEN, RELEASE_VISIBILITY_PUBLIC, ReleasePhaseAdminData,
        ReleasePhaseCreateData, ReleasePhaseUpdateData,
    },
    error::{RbError, RbInternalError},
};

#[derive(Deserialize)]
struct GamePathInfo {
    game_id: i32,
}

#[derive(Deserialize)]
struct PhasePathInfo {
    game_id: i32,
    phase_id: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum ReleaseAdminResult {
    Conflict = -3,
    Invalid = -2,
    Ok = 0,
}

#[derive(Serialize)]
struct ReleaseListResponse {
    code: ReleaseAdminResult,
    phases: Vec<ReleasePhaseAdminData>,
}

#[derive(Serialize)]
struct ReleaseResponse {
    code: ReleaseAdminResult,
    phase: ReleasePhaseAdminData,
}

#[derive(Serialize)]
struct ReleaseDeleteResponse {
    code: ReleaseAdminResult,
}

fn valid_visibility(value: i16) -> bool {
    matches!(value, RELEASE_VISIBILITY_HIDDEN | RELEASE_VISIBILITY_PUBLIC)
}

fn valid_title(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 120
}

fn valid_description(value: &str) -> bool {
    value.chars().count() <= 20_000
}

fn is_constraint_error(error: &RbInternalError) -> bool {
    matches!(
        error,
        RbInternalError::Sql(sqlx::Error::Database(error))
            if error.code().is_some_and(|code| code == "23505" || code == "23514" || code == "23503")
    )
}

async fn list(path: web::Path<GamePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(ReleaseListResponse {
        code: ReleaseAdminResult::Ok,
        phases: release::list_admin(&app.db, path.game_id).await?,
    }))
}

async fn append(
    path: web::Path<GamePathInfo>,
    body: web::Json<ReleasePhaseCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !valid_title(&body.title)
        || !valid_description(&body.description)
        || !valid_visibility(body.visibility)
        || !crate::db::feature::valid_changes(&body.feature_changes)
        || body.release_at <= OffsetDateTime::now_utc()
    {
        return RbError::bad_req(ReleaseAdminResult::Invalid.into()).http_err();
    }
    let phase = match release::create_admin(&app.db, path.game_id, &body).await {
        Ok(Some(phase)) => phase,
        Ok(None) => return RbError::not_found().http_err(),
        Err(error) if is_constraint_error(&error) => {
            return RbError::conflict(ReleaseAdminResult::Conflict.into()).http_err();
        }
        Err(error) => return Err(error.into()),
    };
    app.release_schedule_changed.notify_one();
    app.sync_hub
        .notify_game_release_updated(
            path.game_id,
            release::release_cursor(&app.db, path.game_id).await?,
            true,
        )
        .await;
    Ok(HttpResponse::Ok().json(ReleaseResponse {
        code: ReleaseAdminResult::Ok,
        phase,
    }))
}

async fn edit(
    path: web::Path<PhasePathInfo>,
    body: web::Json<ReleasePhaseUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let Some(current) = release::get_admin(&app.db, path.game_id, path.phase_id).await? else {
        return RbError::not_found().http_err();
    };
    if (current.released
        && (body.content_type.is_some()
            || body.release_at.is_some()
            || body.visibility.is_some()
            || body.feature_changes.is_some()))
        || body
            .title
            .as_deref()
            .is_some_and(|title| !valid_title(title))
        || body
            .description
            .as_deref()
            .is_some_and(|description| !valid_description(description))
        || body
            .visibility
            .is_some_and(|value| !valid_visibility(value))
        || body
            .release_at
            .is_some_and(|release_at| release_at <= OffsetDateTime::now_utc())
        || body
            .feature_changes
            .as_deref()
            .is_some_and(|changes| !crate::db::feature::valid_changes(changes))
    {
        return RbError::bad_req(ReleaseAdminResult::Invalid.into()).http_err();
    }
    let phase = match release::update_admin(&app.db, path.game_id, path.phase_id, &body).await {
        Ok(Some(phase)) => phase,
        Ok(None) => return RbError::not_found().http_err(),
        Err(error) if is_constraint_error(&error) => {
            return RbError::conflict(ReleaseAdminResult::Conflict.into()).http_err();
        }
        Err(error) => return Err(error.into()),
    };
    app.release_schedule_changed.notify_one();
    app.sync_hub
        .notify_game_release_updated(
            path.game_id,
            release::release_cursor(&app.db, path.game_id).await?,
            true,
        )
        .await;
    Ok(HttpResponse::Ok().json(ReleaseResponse {
        code: ReleaseAdminResult::Ok,
        phase,
    }))
}

async fn delete(path: web::Path<PhasePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    if !release::delete_admin(&app.db, path.game_id, path.phase_id).await? {
        return RbError::bad_req(ReleaseAdminResult::Invalid.into()).http_err();
    }
    app.release_schedule_changed.notify_one();
    app.sync_hub
        .notify_game_release_updated(
            path.game_id,
            release::release_cursor(&app.db, path.game_id).await?,
            true,
        )
        .await;
    Ok(HttpResponse::Ok().json(ReleaseDeleteResponse {
        code: ReleaseAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{game_id}/release-phases")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{phase_id}", web::patch().to(edit))
            .route("/{phase_id}", web::delete().to(delete)),
    );
}

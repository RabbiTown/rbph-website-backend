use actix_web::{HttpResponse, Result, web};
use serde::{Deserialize, Serialize};

use crate::{AppState, db, error::RbError, extractor::auth::AuthUser};

#[derive(Deserialize)]
struct NotificationListQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
struct NotificationPath {
    notification_id: i64,
}

#[derive(Deserialize)]
struct NotificationReadManyRequest {
    notification_ids: Vec<i64>,
}

#[derive(Serialize)]
struct NotificationUnreadResponse {
    count: i64,
    dm_count: i64,
}

async fn list_notifications(
    query: web::Query<NotificationListQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let notifications = db::notification::list_for_team(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        query.before,
        query.limit.unwrap_or(20).clamp(1, 100),
    )
    .await?;
    Ok(HttpResponse::Ok().json(notifications))
}

async fn get_notification(
    path: web::Path<NotificationPath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let notification = db::notification::get_for_team(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        path.notification_id,
    )
    .await?
    .ok_or(RbError::not_found())?;
    Ok(HttpResponse::Ok().json(notification))
}

async fn unread_count(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let unread =
        db::notification::unread_count(&app.db, user.req_team_id()?.ok_or(RbError::forbid())?)
            .await?;
    Ok(HttpResponse::Ok().json(NotificationUnreadResponse {
        count: unread.count,
        dm_count: unread.dm_count,
    }))
}

async fn mark_read(
    path: web::Path<NotificationPath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    if db::notification::mark_read(&app.db, team_id, path.notification_id).await? {
        app.sync_hub
            .notify_notification_updated(&app.db, team_id, Some(path.notification_id), "read")
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}

async fn mark_many_read(
    req: web::Json<NotificationReadManyRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    if db::notification::mark_many_read(&app.db, team_id, &req.notification_ids).await? {
        app.sync_hub
            .notify_notification_updated(&app.db, team_id, None, "read")
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}

async fn mark_all_read(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    if db::notification::mark_all_read(&app.db, team_id).await? {
        app.sync_hub
            .notify_notification_updated(&app.db, team_id, None, "read_all")
            .await?;
    }
    Ok(HttpResponse::NoContent().finish())
}

pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_notifications))
        .route("/unread", web::get().to(unread_count))
        .route("/read", web::post().to(mark_many_read))
        .route("/read-all", web::post().to(mark_all_read))
        .route("/{notification_id}", web::get().to(get_notification))
        .route("/{notification_id}/read", web::post().to(mark_read));
}

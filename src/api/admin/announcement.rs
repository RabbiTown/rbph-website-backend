use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use std::collections::HashSet;

use crate::{AppState, db, error::RbError, model::game::RbContentType};

#[derive(Deserialize)]
struct AnnouncementPath {
    announcement_id: i32,
}

#[derive(Deserialize)]
struct AnnouncementListQuery {
    game_id: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum AnnouncementAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct AnnouncementResponse {
    code: AnnouncementAdminResult,
    announcement: db::anmt::AdminAnnouncementData,
}

#[derive(Serialize)]
struct AnnouncementListResponse {
    code: AnnouncementAdminResult,
    announcements: Vec<db::anmt::AdminAnnouncementData>,
}

#[derive(Serialize)]
struct AnnouncementDeleteResponse {
    code: AnnouncementAdminResult,
}

fn valid_content_type(content_type: RbContentType) -> bool {
    matches!(
        content_type,
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

async fn valid_write(app: &AppState, data: &db::anmt::AnnouncementWriteData) -> Result<bool> {
    if data.title.trim().is_empty()
        || data.title.chars().count() > 120
        || !valid_content_type(data.content_type)
    {
        return Ok(false);
    }

    if data
        .puzzle_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>()
        .len()
        != data.puzzle_ids.len()
    {
        return Ok(false);
    }

    match data.game_id {
        None => Ok(data.puzzle_ids.is_empty()),
        Some(game_id) => {
            if !db::game::exists(&app.db, game_id, crate::model::user::RbUserRole::Admin).await? {
                return Ok(false);
            }
            let count = sqlx::query_scalar!(
                "SELECT COUNT(*)
                FROM rb_puzzle p
                JOIN rb_round r ON r.id = p.round_id
                WHERE r.game_id = $1 AND p.id = ANY($2);",
                game_id,
                &data.puzzle_ids
            )
            .fetch_one(&app.db)
            .await
            .map_err(crate::error::RbInternalError::from)?
            .unwrap_or_default();
            Ok(count == data.puzzle_ids.len() as i64)
        }
    }
}

async fn notify_change(app: &AppState, game_id: Option<i32>) {
    app.sync_hub.notify_game_announcement_updated(game_id).await;
}

async fn list(
    query: web::Query<AnnouncementListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(AnnouncementListResponse {
        code: AnnouncementAdminResult::Ok,
        announcements: db::anmt::admin_list(&app.db, query.game_id).await?,
    }))
}

async fn create(
    body: web::Json<db::anmt::AnnouncementWriteData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !valid_write(&app, &body).await? {
        return RbError::bad_req(AnnouncementAdminResult::Invalid.into()).http_err();
    }
    let announcement = db::anmt::admin_create(&app.db, &body).await?;
    notify_change(&app, announcement.game_id).await;
    Ok(HttpResponse::Ok().json(AnnouncementResponse {
        code: AnnouncementAdminResult::Ok,
        announcement,
    }))
}

async fn update(
    path: web::Path<AnnouncementPath>,
    body: web::Json<db::anmt::AnnouncementWriteData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !valid_write(&app, &body).await? {
        return RbError::bad_req(AnnouncementAdminResult::Invalid.into()).http_err();
    }
    let previous = db::anmt::admin_get(&app.db, path.announcement_id).await?;
    let Some(previous) = previous else {
        return RbError::not_found()
            .code(AnnouncementAdminResult::NotFound.into())
            .http_err();
    };
    let announcement = db::anmt::admin_update(&app.db, path.announcement_id, &body)
        .await?
        .ok_or_else(|| RbError::not_found().code(AnnouncementAdminResult::NotFound.into()))?;
    notify_change(&app, previous.game_id).await;
    if previous.game_id != announcement.game_id {
        notify_change(&app, announcement.game_id).await;
    }
    Ok(HttpResponse::Ok().json(AnnouncementResponse {
        code: AnnouncementAdminResult::Ok,
        announcement,
    }))
}

async fn delete(
    path: web::Path<AnnouncementPath>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let announcement = db::anmt::admin_get(&app.db, path.announcement_id).await?;
    let Some(announcement) = announcement else {
        return RbError::not_found()
            .code(AnnouncementAdminResult::NotFound.into())
            .http_err();
    };
    if !db::anmt::admin_delete(&app.db, path.announcement_id).await? {
        return RbError::not_found()
            .code(AnnouncementAdminResult::NotFound.into())
            .http_err();
    }
    notify_change(&app, announcement.game_id).await;
    Ok(HttpResponse::Ok().json(AnnouncementDeleteResponse {
        code: AnnouncementAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("announcements")
            .route("", web::get().to(list))
            .route("", web::post().to(create))
            .route("/{announcement_id}", web::patch().to(update))
            .route("/{announcement_id}", web::delete().to(delete)),
    );
}

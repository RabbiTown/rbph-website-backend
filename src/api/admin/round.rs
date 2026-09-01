use actix_web::{HttpResponse, Result, web};
use deadpool_redis::redis::AsyncCommands;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        round::{RbRoundAdminData, RbRoundCreateData, RbRoundUpdateData},
    },
    error::{RbError, RbInternalError},
    model::game::RbContentType,
};

fn is_constraint_error(err: &RbInternalError) -> bool {
    matches!(
        err,
        RbInternalError::Sql(sqlx::Error::Database(db_err))
            if db_err.code().is_some_and(|code| code == "23505" || code == "23514")
    )
}

#[derive(Deserialize)]
struct RoundPathInfo {
    round_id: i32,
}

#[derive(Deserialize)]
struct RoundListQuery {
    game_id: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum RoundAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct RoundAdminResponse {
    code: RoundAdminResult,
    round: RbRoundAdminData,
}

#[derive(Serialize)]
struct RoundAdminListResponse {
    code: RoundAdminResult,
    rounds: Vec<RbRoundAdminData>,
}

#[derive(Serialize)]
struct RoundAdminDeleteResponse {
    code: RoundAdminResult,
}

fn validate_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

fn validate_slug(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn validate_slug_option(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(validate_slug)
}

fn validate_create(data: &RbRoundCreateData) -> bool {
    validate_content_type(data.content_type) && validate_slug_option(&data.slug)
}

fn validate_update(data: &RbRoundUpdateData) -> bool {
    if let Some(slug) = &data.slug
        && !validate_slug_option(slug)
    {
        return false;
    }

    true
}

async fn invalidate_round_cache(app: &AppState, round_id: i32) {
    if let Ok(mut conn) = app.kv.get().await {
        let _: Result<(), _> = conn.del(format!("round:{round_id}:show:v2")).await;
    }

    let _ = db::cache::del_pattern(&app.kv, &format!("round:{round_id}:team:*:full_state")).await;
}

async fn list(query: web::Query<RoundListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let rounds = db::round::admin_list(&app.db, query.game_id).await?;

    Ok(HttpResponse::Ok().json(RoundAdminListResponse {
        code: RoundAdminResult::Ok,
        rounds,
    }))
}

async fn get(path: web::Path<RoundPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let round = db::round::admin_get(&app.db, path.round_id).await?;
    let Some(round) = round else {
        return RbError::not_found()
            .code(RoundAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(RoundAdminResponse {
        code: RoundAdminResult::Ok,
        round,
    }))
}

async fn append(
    req: web::Json<RbRoundCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_create(&req) {
        return RbError::bad_req(RoundAdminResult::Invalid.into()).http_err();
    }

    let round = match db::round::admin_create(&app.db, &req).await {
        Ok(round) => round,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(RoundAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(round) = round else {
        return RbError::not_found()
            .code(RoundAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_round_cache(&app, round.id).await;

    Ok(HttpResponse::Ok().json(RoundAdminResponse {
        code: RoundAdminResult::Ok,
        round,
    }))
}

async fn edit(
    path: web::Path<RoundPathInfo>,
    req: web::Json<RbRoundUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_update(&req) {
        return RbError::bad_req(RoundAdminResult::Invalid.into()).http_err();
    }

    let round = match db::round::admin_update(&app.db, path.round_id, &req).await {
        Ok(round) => round,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(RoundAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(round) = round else {
        return RbError::not_found()
            .code(RoundAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_round_cache(&app, path.round_id).await;

    Ok(HttpResponse::Ok().json(RoundAdminResponse {
        code: RoundAdminResult::Ok,
        round,
    }))
}

async fn delete(path: web::Path<RoundPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let deleted = db::round::admin_delete(&app.db, path.round_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(RoundAdminResult::NotFound.into())
            .http_err();
    }
    invalidate_round_cache(&app, path.round_id).await;

    Ok(HttpResponse::Ok().json(RoundAdminDeleteResponse {
        code: RoundAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("rounds")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{round_id}", web::get().to(get))
            .route("/{round_id}", web::patch().to(edit))
            .route("/{round_id}", web::delete().to(delete)),
    );
}

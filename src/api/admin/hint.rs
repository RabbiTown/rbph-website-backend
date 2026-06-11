use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        puzzle::{RbHintAdminData, RbHintCreateData, RbHintUpdateData},
    },
    error::{RbError, RbInternalError},
    model::game::RbContentType,
};

fn is_constraint_error(err: &RbInternalError) -> bool {
    matches!(
        err,
        RbInternalError::Sql(sqlx::Error::Database(db_err))
            if db_err.code().is_some_and(|code| code == "23503" || code == "23514")
    )
}

#[derive(Deserialize)]
struct HintPathInfo {
    hint_id: i32,
}

#[derive(Deserialize)]
struct HintListQuery {
    puzzle_id: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum HintAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct HintAdminResponse {
    code: HintAdminResult,
    hint: RbHintAdminData,
}

#[derive(Serialize)]
struct HintAdminListResponse {
    code: HintAdminResult,
    hints: Vec<RbHintAdminData>,
}

#[derive(Serialize)]
struct HintAdminDeleteResponse {
    code: HintAdminResult,
}

fn validate_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

fn validate_basic(
    title: Option<&str>,
    content: Option<&str>,
    content_type: Option<i16>,
    cooldown: Option<i32>,
    cost_amount: Option<i32>,
) -> bool {
    title.is_none_or(|value| !value.trim().is_empty() && value.chars().count() <= 120)
        && content.is_none_or(|_| true)
        && content_type.is_none_or(validate_content_type)
        && cooldown.is_none_or(|value| value >= 0)
        && cost_amount.is_none_or(|value| value >= 0)
}

async fn get_hint_game(app: &AppState, puzzle_id: i32) -> Result<Option<i32>, RbInternalError> {
    db::puzzle::get_puzzle_game(&app.db, puzzle_id).await
}

async fn validate_create(app: &AppState, data: &RbHintCreateData) -> Result<bool, RbInternalError> {
    if !validate_basic(
        Some(&data.title),
        Some(&data.content),
        Some(data.content_type),
        Some(data.cooldown),
        Some(data.cost_amount),
    ) {
        return Ok(false);
    }

    let Some(game_id) = get_hint_game(app, data.puzzle_id).await? else {
        return Ok(false);
    };

    if let Some(cost_id) = data.cost_id {
        if cost_id <= 0 || !db::game::currency_belongs_to_game(&app.db, game_id, cost_id).await? {
            return Ok(false);
        }
    } else if data.cost_amount != 0 {
        return Ok(false);
    }

    Ok(true)
}

async fn validate_update(
    app: &AppState,
    current: &RbHintAdminData,
    data: &RbHintUpdateData,
) -> Result<bool, RbInternalError> {
    if !validate_basic(
        data.title.as_deref(),
        data.content.as_deref(),
        data.content_type,
        data.cooldown,
        data.cost_amount,
    ) {
        return Ok(false);
    }

    let puzzle_id = data.puzzle_id.unwrap_or(current.puzzle_id);
    let Some(game_id) = get_hint_game(app, puzzle_id).await? else {
        return Ok(false);
    };

    let cost_id = data.cost_id.unwrap_or(current.cost_id);
    if let Some(cost_id) = cost_id {
        if cost_id <= 0 || !db::game::currency_belongs_to_game(&app.db, game_id, cost_id).await? {
            return Ok(false);
        }
    }

    Ok(true)
}

async fn invalidate_hint_cache(app: &AppState, puzzle_id: i32) {
    if let Ok(mut conn) = app.kv.get().await {
        use deadpool_redis::redis::AsyncCommands;
        let _: Result<(), _> = conn.del(format!("puzzle:{puzzle_id}:hints")).await;
    }
    let _ = db::cache::del_pattern(&app.kv, &format!("puzzle:{puzzle_id}:team:*:hints")).await;
}

async fn list(query: web::Query<HintListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hints = db::puzzle::admin_list_hints(&app.db, query.puzzle_id).await?;

    Ok(HttpResponse::Ok().json(HintAdminListResponse {
        code: HintAdminResult::Ok,
        hints,
    }))
}

async fn get(path: web::Path<HintPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hint = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn append(
    req: web::Json<RbHintCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_create(&app, &req).await? {
        return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
    }

    let hint = match db::puzzle::admin_create_hint(&app.db, &req).await {
        Ok(hint) => hint,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_hint_cache(&app, hint.puzzle_id).await;

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn edit(
    path: web::Path<HintPathInfo>,
    req: web::Json<RbHintUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let current = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(current) = current else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    if !validate_update(&app, &current, &req).await? {
        return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
    }

    let hint = match db::puzzle::admin_update_hint(&app.db, path.hint_id, &req).await {
        Ok(hint) => hint,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_hint_cache(&app, current.puzzle_id).await;
    if hint.puzzle_id != current.puzzle_id {
        invalidate_hint_cache(&app, hint.puzzle_id).await;
    }

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn delete(path: web::Path<HintPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hint = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    let deleted = db::puzzle::admin_delete_hint(&app.db, path.hint_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    }
    invalidate_hint_cache(&app, hint.puzzle_id).await;

    Ok(HttpResponse::Ok().json(HintAdminDeleteResponse {
        code: HintAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("hints")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{hint_id}", web::get().to(get))
            .route("/{hint_id}", web::patch().to(edit))
            .route("/{hint_id}", web::delete().to(delete)),
    );
}

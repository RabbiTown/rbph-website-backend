use actix_session::SessionExt;
use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, api::error_handler, db, error::RbError, extractor::auth::AuthUser,
    model::user::RbUserRole,
};

#[derive(Deserialize)]
struct RoundPathInfo {
    round_id: i32,
}

#[derive(Serialize)]
struct RoundContentsResponse {
    code: i32,
    contents: Vec<db::content::RbContentBlockShowData>,
}

async fn get_contents(
    path: web::Path<RoundPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let game_id = db::round::get_round_game(&app.db, path.round_id)
        .await?
        .ok_or_else(RbError::not_found)?;
    let contents = db::content::visible_for_team(
        &app.db,
        Some(&app.storage),
        app.settings.storage.content_cdn_backend.is_some(),
        team_id,
        None,
        Some(path.round_id),
        game_id,
    )
    .await?;
    Ok(HttpResponse::Ok().json(RoundContentsResponse { code: 0, contents }))
}

async fn get_round(
    path: web::Path<RoundPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    get_round_response_for_user_team(app.get_ref(), &user, path.round_id).await
}

pub(super) async fn get_round_response_for_user_team(
    app: &AppState,
    user: &AuthUser,
    round_id: i32,
) -> Result<HttpResponse> {
    let result = db::round::get_info_for_team_str(
        &app.db,
        &app.kv,
        round_id,
        user.req_team_id()?.ok_or(RbError::forbid())?,
    )
    .await?;
    let Some(result) = result else {
        return RbError::not_found().http_err();
    };
    let mut response = serde_json::from_str::<serde_json::Value>(&result)?;
    let game_id = response
        .pointer("/data/game_id")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(RbError::not_found)?;
    let renderer = super::game::resolve_frontend_renderer(
        app,
        game_id,
        db::frontend::ROUND_PAGE,
        Some(round_id),
        None,
        None,
    )
    .await?;
    response
        .as_object_mut()
        .ok_or_else(RbError::not_found)?
        .insert("renderer".to_owned(), serde_json::to_value(renderer)?);
    Ok(HttpResponse::Ok().json(response))
}

async fn check_round_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let round_id: i32 = req
        .match_info()
        .get("round_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(RbError::not_found)?;

    let app = req.app_data::<web::Data<AppState>>().unwrap();
    let role = req
        .extensions()
        .get::<RbUserRole>()
        .copied()
        .ok_or_else(RbError::forbid)?;

    match db::round::get_round_user_info(&app.db, user_id, round_id, role).await? {
        Some(info) => {
            req.extensions_mut().insert(info);
        }
        None => {
            RbError::not_found().err()?;
        }
    };

    next.call(req).await
}

// /rounds/...
pub fn rounds_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{round_id}")
            .wrap(middleware::from_fn(check_round_middleware))
            .route("", web::get().to(get_round))
            .route("/contents", web::get().to(get_contents))
            .default_service(web::route().to(error_handler)),
    );
}

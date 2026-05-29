use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{self, game::RbGameUpdateData},
    error::RbError,
    model::game::{RbGame, RbGameSettings},
};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum GameAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct GameAdminResponse {
    code: GameAdminResult,
    game: RbGame,
}

#[derive(Serialize)]
struct GameAdminListResponse {
    code: GameAdminResult,
    games: Vec<RbGame>,
}

async fn list(app: web::Data<AppState>) -> Result<HttpResponse> {
    let games = db::game::list_all(&app.db, false, false).await?;

    Ok(HttpResponse::Ok().json(GameAdminListResponse {
        code: GameAdminResult::Ok,
        games,
    }))
}

async fn get(path: web::Path<PathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let game = db::game::get_full_by_id(&app.db, path.game_id).await?;
    let Some(game) = game else {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(GameAdminResponse {
        code: GameAdminResult::Ok,
        game,
    }))
}

async fn append(
    req: web::Json<db::game::RbGameCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Some(settings) = &req.settings
        && !RbGameSettings::validate_patch(settings)
    {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let game = db::game::create(&app.db, &req).await?;

    Ok(HttpResponse::Ok().json(GameAdminResponse {
        code: GameAdminResult::Ok,
        game,
    }))
}

async fn edit(
    path: web::Path<PathInfo>,
    req: web::Json<RbGameUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Some(settings) = &req.settings
        && !RbGameSettings::validate_patch(settings)
    {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let game = db::game::update(&app.db, path.game_id, &req).await?;
    let Some(game) = game else {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(GameAdminResponse {
        code: GameAdminResult::Ok,
        game,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list))
        .route("", web::post().to(append))
        .route("/{game_id}", web::get().to(get))
        .route("/{game_id}", web::patch().to(edit));
}

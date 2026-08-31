use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        game::{
            RbAdminPageTitle, RbCurrencyAdminData, RbCurrencyCreateData, RbCurrencyUpdateData,
            RbGameUpdateData,
        },
    },
    error::RbError,
    model::game::{RbGame, RbGameSettings},
};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

#[derive(Deserialize)]
struct CurrencyPathInfo {
    game_id: i32,
    currency_id: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum GameAdminResult {
    Conflict = -3,
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum GamePageTitlesResult {
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

#[derive(Serialize)]
struct GamePageTitlesResponse {
    code: GamePageTitlesResult,
    rounds: Vec<RbAdminPageTitle>,
    puzzles: Vec<RbAdminPageTitle>,
}

#[derive(Serialize)]
struct CurrencyAdminListResponse {
    code: GameAdminResult,
    currencies: Vec<RbCurrencyAdminData>,
}

#[derive(Serialize)]
struct CurrencyAdminResponse {
    code: GameAdminResult,
    currency: RbCurrencyAdminData,
}

fn currency_data_valid(
    name: &str,
    slug: &str,
    prec: i32,
    init_amount: i64,
    max_amount: i64,
) -> bool {
    db::game::valid_currency_data(name, slug, prec, init_amount, max_amount)
}

fn is_unique_violation(error: &crate::error::RbInternalError) -> bool {
    match error {
        crate::error::RbInternalError::Sql(sqlx::Error::Database(err)) => {
            err.code().as_deref() == Some("23505")
        }
        _ => false,
    }
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

async fn list_page_titles(
    path: web::Path<PathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !db::game::exists(&app.db, path.game_id, crate::model::user::RbUserRole::Admin).await? {
        return RbError::not_found()
            .code(GamePageTitlesResult::NotFound.into())
            .http_err();
    }

    let (rounds, puzzles) = db::game::list_admin_page_titles(&app.db, path.game_id).await?;
    Ok(HttpResponse::Ok().json(GamePageTitlesResponse {
        code: GamePageTitlesResult::Ok,
        rounds,
        puzzles,
    }))
}

async fn list_currency(
    path: web::Path<PathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !db::game::exists(&app.db, path.game_id, crate::model::user::RbUserRole::Admin).await? {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    }

    let currencies = db::game::list_currency(&app.db, path.game_id).await?;

    Ok(HttpResponse::Ok().json(CurrencyAdminListResponse {
        code: GameAdminResult::Ok,
        currencies,
    }))
}

async fn create_currency(
    path: web::Path<PathInfo>,
    req: web::Json<RbCurrencyCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !currency_data_valid(
        &req.name,
        &req.slug,
        req.prec,
        req.init_amount,
        req.max_amount,
    ) {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let currency = match db::game::create_currency(&app.db, path.game_id, &req).await {
        Ok(currency) => currency,
        Err(error) if is_unique_violation(&error) => {
            return RbError::conflict(GameAdminResult::Conflict.into()).http_err();
        }
        Err(error) => return Err(error.into()),
    };

    let Some(currency) = currency else {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(CurrencyAdminResponse {
        code: GameAdminResult::Ok,
        currency,
    }))
}

async fn edit_currency(
    path: web::Path<CurrencyPathInfo>,
    req: web::Json<RbCurrencyUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !currency_data_valid(
        &req.name,
        &req.slug,
        req.prec,
        req.init_amount,
        req.max_amount,
    ) {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let currency =
        match db::game::update_currency(&app.db, path.game_id, path.currency_id, &req).await {
            Ok(currency) => currency,
            Err(error) if is_unique_violation(&error) => {
                return RbError::conflict(GameAdminResult::Conflict.into()).http_err();
            }
            Err(error) => return Err(error.into()),
        };

    let Some(currency) = currency else {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(CurrencyAdminResponse {
        code: GameAdminResult::Ok,
        currency,
    }))
}

async fn delete_currency(
    path: web::Path<CurrencyPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let currency = db::game::get_currency(&app.db, path.game_id, path.currency_id).await?;
    let Some(currency) = currency else {
        return RbError::not_found()
            .code(GameAdminResult::NotFound.into())
            .http_err();
    };

    db::game::delete_currency(&app.db, path.game_id, path.currency_id).await?;

    Ok(HttpResponse::Ok().json(CurrencyAdminResponse {
        code: GameAdminResult::Ok,
        currency,
    }))
}

async fn append(
    req: web::Json<db::game::RbGameCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut req = req.into_inner();
    req.title = req.title.trim().to_string();
    if !db::game::valid_game_title(&req.title) {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }
    if let Some(settings) = &req.settings
        && !RbGameSettings::validate_patch(settings)
    {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let game = db::game::create(&app.db, &req).await?;
    app.release_schedule_changed.notify_one();

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
    let mut req = req.into_inner();
    if let Some(title) = &mut req.title {
        *title = title.trim().to_string();
        if !db::game::valid_game_title(title) {
            return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
        }
    }
    if let Some(settings) = &req.settings
        && !RbGameSettings::validate_patch(settings)
    {
        return RbError::bad_req(GameAdminResult::Invalid.into()).http_err();
    }

    let game = match db::game::update(&app.db, path.game_id, &req).await {
        Ok(game) => game,
        Err(error) if is_unique_violation(&error) => {
            return RbError::conflict(GameAdminResult::Conflict.into()).http_err();
        }
        Err(error) => return Err(error.into()),
    };
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
        .route("/{game_id}/page-titles", web::get().to(list_page_titles))
        .route("/{game_id}/currencies", web::get().to(list_currency))
        .route("/{game_id}/currencies", web::post().to(create_currency))
        .route(
            "/{game_id}/currencies/{currency_id}",
            web::patch().to(edit_currency),
        )
        .route(
            "/{game_id}/currencies/{currency_id}",
            web::delete().to(delete_currency),
        )
        .route("/{game_id}", web::patch().to(edit));
}

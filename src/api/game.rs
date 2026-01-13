use actix_session::SessionExt;
use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    http::header::ContentType,
    middleware::{self, Next},
    web::{self},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    api::{error_handler, team},
    db::{
        self,
        game::RbGameShowData,
        round::RbRoundSimpleData,
        team::{RbCurrencyShowData, RbTeamFullData},
    },
    error::RbError,
    extractor::auth::AuthUser,
    middleware::privilege::PrivilegeMiddleware,
    model::user::RbUserRole,
};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

async fn get_info(info: web::Path<PathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::game::get_by_id(&app.db, info.game_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

#[derive(Serialize)]
struct GameAggreInfo {
    game: RbGameShowData,
    team: Option<RbTeamFullData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<Vec<RbCurrencyShowData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rounds: Option<Vec<RbRoundSimpleData>>,

    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    server_time: OffsetDateTime,
}

async fn get_aggre_info(
    info: web::Path<PathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let game = db::game::get_by_id(&app.db, info.game_id).await?;
    if game.is_none() {
        RbError::not_found().err()?;
    }

    let team = db::team::get_by_user_game(&app.db, user.uid, info.game_id).await?;

    let (currency, rounds) = match user.get_team_id() {
        Some(team_id) => (
            Some(db::team::get_currency_info(&app.db, team_id).await?),
            Some(db::round::get_simple_list_for_team(&app, info.game_id, team_id).await?),
        ),
        None => (None, None),
    };

    Ok(HttpResponse::Ok().json(GameAggreInfo {
        game: game.unwrap(),
        team,
        currency,
        rounds,
        server_time: OffsetDateTime::now_utc(),
    }))
}

async fn list_online(app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::game::list_all(&app.db, true, true).await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn get_anmts(
    info: web::Path<PathInfo>,
    user: Option<AuthUser>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.and_then(|x| x.get_team_id());
    if let Some(team_id) = team_id {
        Ok(HttpResponse::Ok().json(db::anmt::list_all_for_team(&app.db, team_id).await?))
    } else {
        Ok(HttpResponse::Ok().json(db::anmt::list_all_for_public(&app.db, info.game_id).await?))
    }
}

async fn get_rounds(
    info: web::Path<PathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.get_team_id();
    if team_id.is_none() {
        RbError::not_found().err()?;
    }
    let team_id = team_id.unwrap();

    let result = db::round::get_simple_list_for_team(&app, info.game_id, team_id).await?;
    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
struct LeaderBoardQuery {
    version: Option<u32>,
}

async fn get_leaderboard(
    req: web::Query<LeaderBoardQuery>,
    info: web::Path<PathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::board::LEADER_BOARD_CACHE
        .get_info_str(&app.db, info.game_id, req.version)
        .await?;

    match result {
        Some(json) => Ok(HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(json)),
        None => Ok(HttpResponse::NotModified().finish()),
    }
}

async fn check_game_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let game_id: i32 = req
        .match_info()
        .get("game_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id = req.get_session().get::<i32>("user_id").ok().flatten();

    let app = req.app_data::<web::Data<AppState>>().unwrap();

    if let Some(user_id) = user_id {
        match db::game::get_game_user_info(&app.db, user_id, game_id).await? {
            Some(info) => {
                req.extensions_mut().insert(info);
            }
            None => {
                RbError::not_found().err()?;
            }
        };
    } else if !db::game::exists(&app.db, game_id, RbUserRole::User).await? {
        RbError::not_found().err()?;
    }

    next.call(req).await
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/online", web::get().to(list_online));
    cfg.service(
        web::scope("/{game_id}")
            .wrap(middleware::from_fn(check_game_middleware))
            .route("", web::get().to(get_info))
            .route("/announcements", web::get().to(get_anmts))
            .service(
                web::scope("")
                    .wrap(PrivilegeMiddleware::new(RbUserRole::User))
                    .service(
                        web::scope("/teams")
                            .configure(team::games_config)
                            .default_service(web::route().to(error_handler)),
                    )
                    .route("/info", web::get().to(get_aggre_info))
                    .route("/rounds", web::get().to(get_rounds))
                    .route("/leaderboard", web::get().to(get_leaderboard))
                    .default_service(web::route().to(error_handler)),
            )
            .default_service(web::route().to(error_handler)),
    );
}

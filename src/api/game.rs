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
    api::{error_handler, notification, puzzle, round, team, ticket},
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
pub struct GamePathInfo {
    pub game_id: i32,
}

#[derive(Deserialize)]
struct GamePuzzlePathInfo {
    game_id: i32,
    puzzle_ref: String,
}

#[derive(Deserialize)]
struct GameRoundPathInfo {
    game_id: i32,
    round_ref: String,
}

async fn get_info(info: web::Path<GamePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::game::get_by_id(&app.db, info.game_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_puzzle(
    path: web::Path<GamePuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzle_id =
        db::puzzle::get_puzzle_id_by_game_ref(&app.db, path.game_id, &path.puzzle_ref).await?;
    let Some(puzzle_id) = puzzle_id else {
        return RbError::not_found().http_err();
    };

    if db::puzzle::get_puzzle_user_info(&app.db, user.uid, puzzle_id)
        .await?
        .is_none()
    {
        return RbError::not_found().http_err();
    }

    puzzle::get_puzzle_response_for_user_team(app.get_ref(), &user, puzzle_id).await
}

async fn get_round(
    path: web::Path<GameRoundPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let round_id =
        db::round::get_round_id_by_game_ref(&app.db, path.game_id, &path.round_ref).await?;
    let Some(round_id) = round_id else {
        return RbError::not_found().http_err();
    };

    if db::round::get_round_user_info(&app.db, user.uid, round_id)
        .await?
        .is_none()
    {
        return RbError::not_found().http_err();
    }

    round::get_round_response_for_user_team(app.get_ref(), &user, round_id).await
}

#[derive(Serialize)]
struct GameAggreInfo {
    game: RbGameShowData,
    team: Option<RbTeamFullData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency: Option<Vec<RbCurrencyShowData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rounds: Option<Vec<RbRoundSimpleData>>,
    release_cursor: i64,
    phases: Vec<db::release::PlayerReleasePhaseData>,
    features: std::collections::BTreeMap<db::feature::GameFeature, db::feature::GameFeatureState>,

    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    server_time: OffsetDateTime,
}

async fn get_aggre_info(
    path: web::Path<GamePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let game = db::game::get_by_id(&app.db, path.game_id).await?;
    if game.is_none() {
        RbError::not_found().err()?;
    }

    let team = db::team::get_by_user_game(&app.db, user.uid, path.game_id).await?;

    let (currency, rounds) = match user.req_team_id()? {
        Some(team_id) => (
            Some(db::team::get_currency_info(&app.db, team_id).await?),
            Some(db::round::get_simple_list_for_team(&app, path.game_id, team_id).await?),
        ),
        None => (None, None),
    };

    Ok(HttpResponse::Ok().json(GameAggreInfo {
        game: game.unwrap(),
        team,
        currency,
        rounds,
        release_cursor: db::release::release_cursor(&app.db, path.game_id).await?,
        phases: db::release::list_player(&app.db, path.game_id).await?,
        features: db::feature::player_states(&app.db, path.game_id).await?,
        server_time: OffsetDateTime::now_utc(),
    }))
}

#[derive(Deserialize)]
struct ReleaseSyncRequest {
    after: i64,
}

#[derive(Serialize)]
struct ReleaseSyncResponse {
    release_cursor: i64,
    events: Vec<db::release::ReleaseSyncEvent>,
    phases: Vec<db::release::PlayerReleasePhaseData>,
    features: std::collections::BTreeMap<db::feature::GameFeature, db::feature::GameFeatureState>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    server_time: OffsetDateTime,
}

async fn sync_releases(
    path: web::Path<GamePathInfo>,
    body: web::Json<ReleaseSyncRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?;
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let events =
        db::release::sync_events(&app.db, path.game_id, team_id, body.after.max(0)).await?;
    let release_cursor = events
        .last()
        .map(|event| event.id)
        .unwrap_or(db::release::release_cursor(&app.db, path.game_id).await?);
    Ok(HttpResponse::Ok().json(ReleaseSyncResponse {
        release_cursor,
        events,
        phases: db::release::list_player(&app.db, path.game_id).await?,
        features: db::feature::player_states(&app.db, path.game_id).await?,
        server_time: OffsetDateTime::now_utc(),
    }))
}

async fn list_online(app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::game::list_show(&app.db, true, true).await?;

    Ok(HttpResponse::Ok().json(result))
}

async fn get_anmts(
    path: web::Path<GamePathInfo>,
    user: Option<AuthUser>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.map(|u| u.req_team_id()).transpose()?.flatten();
    if let Some(team_id) = team_id {
        Ok(HttpResponse::Ok().json(db::anmt::list_all_for_team(&app.db, team_id).await?))
    } else {
        Ok(HttpResponse::Ok().json(db::anmt::list_all_for_public(&app.db, path.game_id).await?))
    }
}

async fn get_rounds(
    path: web::Path<GamePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?;
    if team_id.is_none() {
        RbError::not_found().err()?;
    }
    let team_id = team_id.unwrap();

    let result = db::round::get_simple_list_for_team(&app, path.game_id, team_id).await?;
    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
struct LeaderBoardQuery {
    version: Option<u32>,
}

async fn get_leaderboard(
    path: web::Path<GamePathInfo>,
    req: web::Query<LeaderBoardQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let result = db::board::LEADER_BOARD_CACHE
        .get_info_str(&app.db, path.game_id, req.version)
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
                    .service(
                        web::scope("/tickets")
                            .configure(ticket::games_config)
                            .default_service(web::route().to(error_handler)),
                    )
                    .service(
                        web::scope("/notifications")
                            .configure(notification::games_config)
                            .default_service(web::route().to(error_handler)),
                    )
                    .route("/info", web::get().to(get_aggre_info))
                    .route("/releases/sync", web::post().to(sync_releases))
                    .route("/rounds", web::get().to(get_rounds))
                    .route("/rounds/{round_ref}", web::get().to(get_round))
                    .route("/puzzles/{puzzle_ref}", web::get().to(get_puzzle))
                    .route("/leaderboard", web::get().to(get_leaderboard))
                    .default_service(web::route().to(error_handler)),
            )
            .default_service(web::route().to(error_handler)),
    );
}

use actix_session::SessionExt;
use actix_web::{
    HttpMessage, HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web::{self},
};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
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

#[derive(Deserialize)]
struct FrontendRendererQuery {
    surface: String,
    round_id: Option<i32>,
    puzzle_id: Option<i32>,
    preview: Option<i64>,
}

#[derive(Deserialize)]
struct FrontendFeaturesQuery {
    preview: Option<i64>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub(super) enum FrontendRendererResult {
    InvalidQuery = -1,
    ResourceNotFound = -2,
    PreviewForbidden = -3,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendFeaturesResult {
    PreviewForbidden = -1,
    Ok = 0,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FrontendRendererResponse {
    code: FrontendRendererResult,
    mode: &'static str,
    layout: &'static str,
    surface: String,
    revision: Option<i64>,
    package_id: Option<i32>,
    renderer_id: Option<String>,
    manifest_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendFeaturePackage {
    package_id: i32,
    manifest_url: String,
    features: Vec<db::frontend::FrontendFeature>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendFeaturesResponse {
    code: FrontendFeaturesResult,
    revision: Option<i64>,
    packages: Vec<FrontendFeaturePackage>,
}

async fn get_frontend_renderer(
    path: web::Path<GamePathInfo>,
    query: web::Query<FrontendRendererQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let user_role = user.req_role()?;
    if query.preview.is_some_and(|_| !user_role.is_admin()) {
        return RbError::forbid()
            .code(FrontendRendererResult::PreviewForbidden.into())
            .http_err();
    }
    if !matches!(
        query.surface.as_str(),
        db::frontend::ROUND_PAGE | db::frontend::PUZZLE_PAGE
    ) || (query.surface == db::frontend::ROUND_PAGE && query.puzzle_id.is_some())
        || (query.surface == db::frontend::ROUND_PAGE && query.round_id.is_none())
        || (query.surface == db::frontend::PUZZLE_PAGE && query.puzzle_id.is_none())
    {
        return RbError::bad_req(FrontendRendererResult::InvalidQuery.into()).http_err();
    }

    let (round_id, puzzle_id) = match query.surface.as_str() {
        db::frontend::ROUND_PAGE => {
            let round_id = query.round_id.expect("round-page requires round_id");
            let user_info =
                db::round::get_round_user_info(&app.db, user.uid, round_id, user_role).await?;
            if !matches!(user_info, Some(info) if info.game_id == path.game_id) {
                return RbError::not_found()
                    .code(FrontendRendererResult::ResourceNotFound.into())
                    .http_err();
            }
            (Some(round_id), None)
        }
        db::frontend::PUZZLE_PAGE => {
            let puzzle_id = query.puzzle_id.expect("puzzle-page requires puzzle_id");
            let user_info =
                db::puzzle::get_puzzle_user_info(&app.db, user.uid, puzzle_id, user_role).await?;
            if !matches!(user_info, Some(info) if info.game_id == path.game_id) {
                return RbError::not_found()
                    .code(FrontendRendererResult::ResourceNotFound.into())
                    .http_err();
            }

            let Some(actual_round_id) = db::puzzle::get_puzzle_round(&app.db, puzzle_id).await?
            else {
                return RbError::not_found()
                    .code(FrontendRendererResult::ResourceNotFound.into())
                    .http_err();
            };
            // Resolve bindings against the puzzle's real round. Trusting a caller-supplied
            // round ID here could expose a locked round's package through an accessible puzzle.
            if query
                .round_id
                .is_some_and(|round_id| round_id != actual_round_id)
            {
                return RbError::not_found()
                    .code(FrontendRendererResult::ResourceNotFound.into())
                    .http_err();
            }
            (Some(actual_round_id), Some(puzzle_id))
        }
        _ => unreachable!("surface was validated above"),
    };

    let response = resolve_frontend_renderer(
        app.get_ref(),
        path.game_id,
        &query.surface,
        round_id,
        puzzle_id,
        query.preview,
    )
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

pub(super) async fn resolve_frontend_renderer(
    app: &AppState,
    game_id: i32,
    surface: &str,
    round_id: Option<i32>,
    puzzle_id: Option<i32>,
    preview_revision: Option<i64>,
) -> Result<FrontendRendererResponse> {
    let row = db::frontend::resolve_cached(
        &app.db,
        &app.kv,
        game_id,
        surface,
        round_id,
        puzzle_id,
        preview_revision,
    )
    .await?;
    let Some(row) = row else {
        return Ok(FrontendRendererResponse {
            code: FrontendRendererResult::Ok,
            mode: "builtin",
            layout: "game",
            surface: surface.to_owned(),
            revision: None,
            package_id: None,
            renderer_id: None,
            manifest_url: None,
        });
    };
    let manifest_url = match (
        &row.package_id,
        &row.backend,
        &row.object_key,
        &row.manifest_path,
    ) {
        (Some(_), Some(backend), Some(object_key), Some(manifest_path)) => app
            .storage
            .asset_public_url(backend, object_key, manifest_path),
        _ => None,
    };
    let mode = if row.package_id.is_some() && row.renderer_id.is_some() && manifest_url.is_some() {
        "package"
    } else {
        "builtin"
    };
    let layout = if mode == "package"
        && row
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.get("features"))
            .and_then(|features| features.get("renderers"))
            .and_then(|renderers| row.renderer_id.as_ref().and_then(|id| renderers.get(id)))
            .and_then(|renderer| renderer.get("layout"))
            .and_then(serde_json::Value::as_str)
            == Some("game-full")
    {
        "game-full"
    } else {
        "game"
    };
    Ok(FrontendRendererResponse {
        code: FrontendRendererResult::Ok,
        mode,
        layout,
        surface: surface.to_owned(),
        revision: Some(row.revision),
        package_id: row.package_id,
        renderer_id: row.renderer_id,
        manifest_url,
    })
}

async fn get_frontend_features(
    path: web::Path<GamePathInfo>,
    query: web::Query<FrontendFeaturesQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let user_role = user.req_role()?;
    if query.preview.is_some_and(|_| !user_role.is_admin()) {
        return RbError::forbid()
            .code(FrontendFeaturesResult::PreviewForbidden.into())
            .http_err();
    }
    let rows = db::frontend::resolve_features(&app.db, path.game_id, query.preview).await?;
    let revision = rows.first().map(|row| row.revision);
    let mut packages = std::collections::BTreeMap::<i32, FrontendFeaturePackage>::new();
    for row in rows {
        let Some(manifest_url) =
            app.storage
                .asset_public_url(&row.backend, &row.object_key, &row.manifest_path)
        else {
            continue;
        };
        packages
            .entry(row.package_id)
            .or_insert_with(|| FrontendFeaturePackage {
                package_id: row.package_id,
                manifest_url,
                features: Vec::new(),
            })
            .features
            .push(row.feature);
    }
    Ok(HttpResponse::Ok().json(FrontendFeaturesResponse {
        code: FrontendFeaturesResult::Ok,
        revision,
        packages: packages.into_values().collect(),
    }))
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

    if db::puzzle::get_puzzle_user_info(&app.db, user.uid, puzzle_id, user.req_role()?)
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

    if db::round::get_round_user_info(&app.db, user.uid, round_id, user.req_role()?)
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
    let sync = db::release::sync_events(&app.db, path.game_id, team_id, body.after.max(0)).await?;
    Ok(HttpResponse::Ok().json(ReleaseSyncResponse {
        release_cursor: sync.cursor,
        events: sync.events,
        phases: db::release::list_player(&app.db, path.game_id).await?,
        features: db::feature::player_states(&app.db, path.game_id).await?,
        server_time: OffsetDateTime::now_utc(),
    }))
}

async fn list_active(user: Option<AuthUser>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::game::list_accessible_show(&app.db, user.map(|user| user.uid)).await?;

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
    version: Option<i64>,
    offset: Option<usize>,
    limit: Option<usize>,
}

async fn get_leaderboard(
    path: web::Path<GamePathInfo>,
    req: web::Query<LeaderBoardQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let offset = req.offset.unwrap_or(0);
    let limit = req.limit.unwrap_or(50).clamp(1, 100);
    let result = db::board::LEADER_BOARD_CACHE
        .get_info(&app.db, &app.kv, path.game_id, req.version, offset, limit)
        .await?;

    match result {
        Some(info) => Ok(HttpResponse::Ok().json(info)),
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
        let auth = db::user::get_auth_state_by_id(&app.db, user_id)
            .await?
            .ok_or_else(RbError::not_found)?;
        let role = auth.role;
        match db::game::get_game_user_info(&app.db, user_id, game_id, role).await? {
            Some(info) => {
                req.extensions_mut().insert(role);
                req.extensions_mut().insert(auth);
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
    cfg.route("/active", web::get().to(list_active));
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
                    .route("/frontend/renderer", web::get().to(get_frontend_renderer))
                    .route("/frontend/features", web::get().to(get_frontend_features))
                    .route("/rounds/{round_ref}", web::get().to(get_round))
                    .route("/puzzles/{puzzle_ref}", web::get().to(get_puzzle))
                    .route("/leaderboard", web::get().to(get_leaderboard))
                    .default_service(web::route().to(error_handler)),
            )
            .default_service(web::route().to(error_handler)),
    );
}

use actix_session::SessionExt;
use actix_web::{
    HttpResponse, Result,
    body::MessageBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web,
};
use num_enum::IntoPrimitive;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use validator::Validate;

use crate::{
    AppState,
    db::{
        self,
        team::{RbTeamPutData, TeamJoinResult},
    },
    error::RbError,
    extractor::auth::AuthUser,
};

#[derive(Deserialize)]
struct GamePathInfo {
    game_id: i32,
}

#[derive(Deserialize)]
struct TeamPathInfo {
    team_id: i32,
}

#[derive(Deserialize)]
struct TeamCreateRequest {
    pub tname: String,
    pub pass: String,
    pub bio: String,
}

#[derive(Serialize)]
struct TeamCreateResponse {
    code: TeamCreateResult,
    tid: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamCreateResult {
    Invalid = -2,
    ToMany = -1,
    Ok = 0,
}

static PWD_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[!-~]{8,32}$").unwrap());

async fn create_self(
    user: AuthUser,
    path: web::Path<GamePathInfo>,
    req: web::Json<TeamCreateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let req = req.into_inner();

    let trimmed_pwd = req.pass.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::unauth()
            .code(TeamCreateResult::Invalid.into())
            .err()?
    }

    let data = RbTeamPutData {
        tname: req.tname.trim().to_string(),
        pass: trimmed_pwd.to_string(),
        bio: req.bio,
        game_id: path.game_id,
    };

    let team_id = db::team::user_create(&app.db, user.uid, &data).await?;
    if team_id.is_none() {
        RbError::conflict(TeamCreateResult::ToMany.into()).err()?
    }

    Ok(HttpResponse::Ok().json(TeamCreateResponse {
        code: TeamCreateResult::Ok,
        tid: team_id.unwrap(),
    }))
}

#[derive(Serialize)]
struct TeamUpdateResponse {
    code: TeamUpdateResult,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamUpdateResult {
    Invalid = -2,
    Bad = -1,
    Ok = 0,
}

async fn update_self(
    req: web::Json<db::team::UserUpdateData>,
    user: AuthUser,
    path: web::Path<GamePathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(e) = req.validate() {
        RbError::bad_req(TeamUpdateResult::Invalid.into())
            .msg(e.to_string())
            .err()?;
    }

    let result = db::team::user_update(&app, path.game_id, user.uid, &req).await?;
    if !result {
        RbError::conflict(TeamUpdateResult::Bad.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamUpdateResponse {
        code: TeamUpdateResult::Ok,
    }))
}

#[derive(Serialize)]
struct TeamLeaveResponse {
    code: TeamLeaveResult,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamLeaveResult {
    Bad = -1,
    Ok = 0,
}

async fn leave_self(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team_id = user
        .get_team_id()
        .ok_or(RbError::conflict(TeamLeaveResult::Bad.into()))?;

    let result = db::team::leave(&app, team_id, user.uid).await?;
    if !result {
        RbError::conflict(TeamLeaveResult::Bad.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamLeaveResponse {
        code: TeamLeaveResult::Ok,
    }))
}

async fn disband_self(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team_id = user.get_team_id().ok_or(RbError::forbid())?;

    let result = db::team::disband(&app, team_id).await?;
    if !result {
        RbError::conflict(TeamLeaveResult::Bad.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamLeaveResponse {
        code: TeamLeaveResult::Ok,
    }))
}

#[derive(Deserialize)]
struct TeamJoinRequest {
    password: String,
}

#[derive(Serialize)]
struct TeamJoinResponse {
    code: TeamJoinResult,
}

async fn join(
    path: web::Path<TeamPathInfo>,
    req: web::Json<TeamJoinRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::team::join(&app, path.team_id, user.uid, &req.password).await?;
    if matches!(result, TeamJoinResult::NotFound) {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(TeamJoinResponse { code: result }))
}

async fn get_self(
    path: web::Path<GamePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::team::get_by_user_game(&app.db, user.uid, path.game_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_self_currency(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team_id = user.get_team_id();
    if team_id.is_none() {
        RbError::not_found().err()?
    }

    let result = db::team::get_currency_info(&app.db, team_id.unwrap()).await?;

    Ok(HttpResponse::Ok().json(result))
}

// -- kick --

#[derive(Deserialize)]
struct TeamTargetRequest {
    target: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamTargetResult {
    TargetSelf = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TeamTargetResponse {
    code: TeamTargetResult,
}

async fn kick_self(
    req: web::Json<TeamTargetRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if req.target == user.uid {
        RbError::conflict(TeamTargetResult::TargetSelf.into()).err()?;
    }

    let team_id = user.get_team_id().ok_or(RbError::forbid())?;

    let result = db::team::kick_member(&app, team_id, req.target).await?;
    if !result {
        RbError::conflict(TeamTargetResult::NotFound.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamTargetResponse {
        code: TeamTargetResult::Ok,
    }))
}

// -- promote --

async fn promote_self(
    req: web::Json<TeamTargetRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if req.target == user.uid {
        RbError::conflict(TeamTargetResult::TargetSelf.into()).err()?;
    }

    let team_id = user.get_team_id().ok_or(RbError::forbid())?;

    let result = db::team::promote_member(&app, team_id, req.target).await?;
    if !result {
        RbError::conflict(TeamTargetResult::NotFound.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamTargetResponse {
        code: TeamTargetResult::Ok,
    }))
}

// TODO : add paging
async fn list_all() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

async fn get_info(req: web::Path<TeamPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::team::get_by_id_show(&app.db, req.team_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn check_leader_middleware(
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

    if let Some(user_id) = user_id
        && db::team::is_leader_in_game(app, game_id, user_id).await?
    {
    } else {
        RbError::forbid().err()?;
    }

    next.call(req).await
}

// /games/{game_id}/teams/...
pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_all))
        .route("/self", web::get().to(get_self))
        .route("/self", web::post().to(create_self))
        .route("/self/leave", web::post().to(leave_self))
        .route("/self/currency", web::get().to(get_self_currency))
        .service(
            web::scope("/self")
                .wrap(middleware::from_fn(check_leader_middleware))
                .route("", web::patch().to(update_self))
                .route("/disband", web::post().to(disband_self))
                .route("/promote", web::post().to(promote_self))
                .route("/kick", web::post().to(kick_self)),
        );
}

// /teams/...
pub fn teams_config(cfg: &mut web::ServiceConfig) {
    cfg.route("/{team_id}", web::get().to(get_info))
        .route("/{team_id}/join", web::post().to(join));
}

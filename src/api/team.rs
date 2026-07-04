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
        team::{RbTeamPutData, TeamCreateResult as TeamCreateDbResult, TeamJoinResult},
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
    pub name: String,
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
    NotOpen = -3,
    Invalid = -2,
    ToMany = -1,
    Ok = 0,
}

static PWD_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[!-~]{8,32}$").unwrap());

async fn create_self(
    path: web::Path<GamePathInfo>,
    req: web::Json<TeamCreateRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let req = req.into_inner();

    let trimmed_pwd = req.pass.trim();
    if !PWD_REGEX.is_match(trimmed_pwd) {
        RbError::bad_req(TeamCreateResult::Invalid.into()).err()?
    }

    let data = RbTeamPutData {
        name: req.name.trim().to_string(),
        pass: trimmed_pwd.to_string(),
        bio: req.bio,
        game_id: path.game_id,
    };

    let team_id = match db::team::user_create(&app.db, user.uid, &data).await? {
        TeamCreateDbResult::NotOpen => {
            return RbError::conflict(TeamCreateResult::NotOpen.into()).http_err();
        }
        TeamCreateDbResult::ToMany => {
            return RbError::conflict(TeamCreateResult::ToMany.into()).http_err();
        }
        TeamCreateDbResult::Ok(team_id) => team_id,
    };

    Ok(HttpResponse::Ok().json(TeamCreateResponse {
        code: TeamCreateResult::Ok,
        tid: team_id,
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
    path: web::Path<GamePathInfo>,
    req: web::Json<db::team::UserUpdateData>,
    user: AuthUser,
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
        .req_team_id()?
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
    let team_id = user.req_team_id()?.ok_or(RbError::not_found())?;

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
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let result = db::team::join(&app, path.team_id, user.uid, &req.password).await?;
    if matches!(result, TeamJoinResult::NotFound) {
        RbError::not_found().err()?
    }

    if !matches!(result, TeamJoinResult::Ok) {
        RbError::conflict(result.into()).err()?
    }

    Ok(HttpResponse::Ok().json(TeamJoinResponse {
        code: TeamJoinResult::Ok,
    }))
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
    let team_id = user.req_team_id()?;
    if team_id.is_none() {
        RbError::not_found().err()?
    }

    let result = db::team::get_currency_info(&app.db, team_id.unwrap()).await?;

    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
struct TeamActivityQuery {
    before: Option<i64>,
    limit: Option<i64>,
    currency_id: Option<i32>,
    include_summary: Option<bool>,
}

async fn get_self_activity(
    user: AuthUser,
    query: web::Query<TeamActivityQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?;
    if team_id.is_none() {
        RbError::not_found().err()?
    }

    let team_id = team_id.unwrap();
    let result = db::event_log::list_team_activity(
        &app.db,
        team_id,
        query.currency_id,
        query.before,
        query.limit.unwrap_or(30),
    )
    .await?;

    if query.include_summary.unwrap_or(false)
        && let Some(currency_id) = query.currency_id
    {
        let summary =
            db::event_log::get_currency_activity_summary(&app.db, team_id, currency_id).await?;
        return Ok(HttpResponse::Ok().json(serde_json::json!({
            "data": result,
            "summary": summary
        })));
    }

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
        RbError::unprocessable(TeamTargetResult::TargetSelf.into()).err()?;
    }

    let team_id = user.req_team_id()?.ok_or(RbError::not_found())?;

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
        RbError::unprocessable(TeamTargetResult::TargetSelf.into()).err()?;
    }

    let team_id = user.req_team_id()?.ok_or(RbError::not_found())?;

    let result = db::team::promote_member(&app, team_id, req.target).await?;
    if !result {
        RbError::conflict(TeamTargetResult::NotFound.into()).err()?;
    }

    Ok(HttpResponse::Ok().json(TeamTargetResponse {
        code: TeamTargetResult::Ok,
    }))
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
    cfg.route("/self", web::get().to(get_self))
        .route("/self", web::post().to(create_self))
        .route("/self/leave", web::post().to(leave_self))
        .route("/self/currency", web::get().to(get_self_currency))
        .route("/self/activity", web::get().to(get_self_activity))
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

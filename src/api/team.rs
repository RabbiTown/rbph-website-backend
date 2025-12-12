use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    DbPool, KvPool,
    db::{self, team::RbTeamPutData},
    error::RbError,
    extractor::auth::AuthUser,
    model::game::RbTeamState,
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
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
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

    let team_id = db::team::user_create(&db_pool, &kv_pool, user.uid, &data).await?;
    if team_id.is_none() {
        RbError::conflict(TeamCreateResult::ToMany.into()).err()?
    }

    Ok(HttpResponse::Ok().json(TeamCreateResponse {
        code: TeamCreateResult::Ok,
        tid: team_id.unwrap(),
    }))
}

async fn update_self(
    user: AuthUser,
    path: web::Path<GamePathInfo>,
    req: web::Json<TeamCreateRequest>,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    // TODO
    Ok(HttpResponse::Ok().finish())
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

async fn leave_self(
    user: AuthUser,
    path: web::Path<GamePathInfo>,
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
) -> Result<HttpResponse> {
    let result = db::team::leave(&db_pool, &kv_pool, path.game_id, user.uid).await?;
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

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamJoinResult {
    WrongPwd = -4,
    TeamFull = -3,
    Locked = -2,
    ToMany = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TeamJoinResponse {
    code: TeamJoinResult,
}

// FIXME : TOCTOU
async fn join(
    path: web::Path<TeamPathInfo>,
    req: web::Json<TeamJoinRequest>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
    kv_pool: web::Data<KvPool>,
) -> Result<HttpResponse> {
    let data = db::team::get_by_id_verify(&db_pool, path.team_id).await?;
    if data.is_none() {
        RbError::not_found().err()?
    }

    let data = data.unwrap();

    if data.tstate == RbTeamState::InGame {
        RbError::conflict(TeamJoinResult::Locked.into()).err()?
    }

    // TODO : make max count configurable
    if data.member_count.unwrap_or_default() >= 6 {
        RbError::conflict(TeamJoinResult::TeamFull.into()).err()?
    }

    if data.pass != req.password {
        RbError::unauth()
            .code(TeamJoinResult::WrongPwd.into())
            .err()?
    }

    let result = db::team::join(&db_pool, &kv_pool, path.team_id, user.uid, false).await?;

    Ok(HttpResponse::Ok().json(TeamJoinResponse {
        code: if result {
            TeamJoinResult::Ok
        } else {
            TeamJoinResult::ToMany
        },
    }))
}

async fn get_self(
    path: web::Path<GamePathInfo>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let result = db::team::get_by_user_game(&db_pool, user.uid, path.game_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

// TODO : add paging
async fn list_all(user: AuthUser, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

async fn get_info(
    req: web::Path<TeamPathInfo>,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let result = db::team::get_by_id_show(&db_pool, req.team_id).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

// /games/{game_id}/teams/...
pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(list_all))
        .route("/self", web::get().to(get_self))
        .route("/self", web::post().to(create_self))
        .route("/self", web::patch().to(update_self))
        .route("/self/leave", web::post().to(leave_self));
}

// /teams/...
pub fn teams_config(cfg: &mut web::ServiceConfig) {
    cfg.route("/{team_id}", web::get().to(get_info))
        .route("/{team_id}/join", web::post().to(join));
}

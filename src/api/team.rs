use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    DbPool,
    db::{self, team::RbTeamPutData},
    error::RbError,
    extractor::auth::AuthUser,
};

#[derive(Deserialize)]
struct PathInfo {
    team_id: i32,
}

#[derive(Serialize)]
struct TeamCreateResponse {
    code: TeamCreateResult,
    tid: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamCreateResult {
    ToMany = -1,
    Ok = 0,
}

async fn create(
    user: AuthUser,
    req: web::Json<RbTeamPutData>,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let count = db::team::count_user_teams(&db_pool, user.uid).await?;
    // TODO : make limit configurable
    if count > 0 {
        RbError::conflict(TeamCreateResult::ToMany.into()).err()?
    }

    let team_id = db::team::append(&db_pool, &req).await?;
    db::team::join(&db_pool, team_id, user.uid, true).await?;

    Ok(HttpResponse::Ok().json(TeamCreateResponse {
        code: TeamCreateResult::Ok,
        tid: team_id,
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
    path: web::Path<PathInfo>,
    req: web::Json<TeamJoinRequest>,
    user: AuthUser,
    db_pool: web::Data<DbPool>,
) -> Result<HttpResponse> {
    let count = db::team::count_user_teams(&db_pool, user.uid).await?;
    // TODO : make limit configurable
    if count > 0 {
        RbError::conflict(TeamJoinResult::ToMany.into()).err()?
    }

    let data = db::team::get_by_id_verify(&db_pool, path.team_id).await?;

    if data.is_none() {
        RbError::not_found().err()?
    }

    let data = data.unwrap();

    if data.locked {
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

    db::team::join(&db_pool, path.team_id, user.uid, false).await?;

    Ok(HttpResponse::Ok().json(TeamJoinResponse {
        code: TeamJoinResult::Ok,
    }))
}

async fn list_self(user: AuthUser, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    let result = db::team::get_by_user(&db_pool, user.uid).await?;
    Ok(HttpResponse::Ok().json(result))
}

// TODO : add paging
async fn list_all(user: AuthUser, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

async fn get_info(req: web::Path<PathInfo>, db_pool: web::Data<DbPool>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/", web::get().to(list_all))
        .route("/self", web::get().to(list_self))
        .route("/self", web::post().to(create))
        .route("/{team_id}", web::get().to(get_info))
        .route("/{team_id}/join", web::post().to(join));
}

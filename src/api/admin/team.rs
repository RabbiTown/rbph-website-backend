use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use validator::Validate;

use crate::{AppState, db, error::RbError, model::game::RbTeam};

#[derive(Deserialize)]
struct PathInfo {
    game_id: i32,
}

#[derive(Deserialize)]
struct TeamPathInfo {
    game_id: i32,
    team_id: i32,
}

#[derive(Deserialize)]
struct MemberPathInfo {
    game_id: i32,
    team_id: i32,
    user_id: i32,
}

#[derive(Deserialize)]
struct CurrencyPathInfo {
    game_id: i32,
    team_id: i32,
    currency_id: i32,
}

#[derive(Deserialize)]
struct TeamListQuery {
    search: Option<String>,
    is_banned: Option<bool>,
    is_locked: Option<bool>,
    is_finished: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct UserSearchQuery {
    search: Option<String>,
}

#[derive(Deserialize)]
struct MemberAddRequest {
    user_id: i32,
}

#[derive(Deserialize)]
struct CurrencyUpdateRequest {
    amount: Option<i64>,
    growth: Option<i64>,
    hidden: Option<bool>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum TeamAdminResult {
    Conflict = -4,
    Invalid = -3,
    LastMember = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TeamListResponse {
    code: TeamAdminResult,
    teams: Vec<db::team::AdminTeamListItem>,
    total: i64,
}

#[derive(Serialize)]
struct TeamResponse {
    code: TeamAdminResult,
    team: db::team::AdminTeamDetail,
}

#[derive(Serialize)]
struct UserSearchResponse {
    code: TeamAdminResult,
    users: Vec<db::team::AdminUserOption>,
}

async fn list(
    path: web::Path<PathInfo>,
    query: web::Query<TeamListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let search = query.search.as_deref().unwrap_or("").trim();
    let filter = db::team::AdminTeamListFilter {
        search,
        is_banned: query.is_banned,
        is_locked: query.is_locked,
        is_finished: query.is_finished,
        limit: query.limit.unwrap_or(50).clamp(1, 200),
        offset: query.offset.unwrap_or(0).max(0),
    };
    let teams = db::team::admin_list(&app.db, path.game_id, filter).await?;
    let total = db::team::admin_count(&app.db, path.game_id, filter).await?;

    Ok(HttpResponse::Ok().json(TeamListResponse {
        code: TeamAdminResult::Ok,
        teams,
        total,
    }))
}

async fn search_users(
    path: web::Path<PathInfo>,
    query: web::Query<UserSearchQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let search = query.search.as_deref().unwrap_or("").trim();
    let users = db::team::admin_search_users(&app.db, path.game_id, search).await?;

    Ok(HttpResponse::Ok().json(UserSearchResponse {
        code: TeamAdminResult::Ok,
        users,
    }))
}

async fn get(path: web::Path<TeamPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team = db::team::admin_get(&app.db, path.game_id, path.team_id).await?;
    let Some(team) = team else {
        return RbError::not_found()
            .code(TeamAdminResult::NotFound.into())
            .http_err();
    };
    Ok(HttpResponse::Ok().json(TeamResponse {
        code: TeamAdminResult::Ok,
        team,
    }))
}

async fn create(
    path: web::Path<PathInfo>,
    req: web::Json<db::team::AdminTeamCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(error) = req.validate() {
        return RbError::bad_req(TeamAdminResult::Invalid.into())
            .msg(error.to_string())
            .http_err();
    }
    let result = db::team::admin_create(&app.db, path.game_id, &req).await?;
    let team_id = match result {
        db::team::AdminTeamCreateResult::UserConflict => {
            return RbError::conflict(TeamAdminResult::Conflict.into()).http_err();
        }
        db::team::AdminTeamCreateResult::NotFound => {
            return RbError::not_found()
                .code(TeamAdminResult::NotFound.into())
                .http_err();
        }
        db::team::AdminTeamCreateResult::Ok(team_id) => team_id,
    };
    let team = db::team::admin_get(&app.db, path.game_id, team_id)
        .await?
        .ok_or(RbError::not_found())?;
    Ok(HttpResponse::Ok().json(TeamResponse {
        code: TeamAdminResult::Ok,
        team,
    }))
}

async fn update(
    path: web::Path<TeamPathInfo>,
    req: web::Json<db::team::AdminTeamUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(error) = req.validate() {
        return RbError::bad_req(TeamAdminResult::Invalid.into())
            .msg(error.to_string())
            .http_err();
    }
    if let Some(features) = &req.features
        && (features.iter().any(|feature| {
            !matches!(
                feature.feature,
                db::feature::GameFeature::DirectMessage
                    | db::feature::GameFeature::PuzzleTicket
                    | db::feature::GameFeature::Leaderboard
            )
        }) || features.len()
            != features
                .iter()
                .map(|feature| feature.feature.value())
                .collect::<std::collections::HashSet<_>>()
                .len())
    {
        return RbError::bad_req(TeamAdminResult::Invalid.into()).http_err();
    }
    let team = db::team::admin_update(&app.db, path.game_id, path.team_id, &req).await?;
    let Some(team) = team else {
        return RbError::not_found()
            .code(TeamAdminResult::NotFound.into())
            .http_err();
    };
    db::cache::invalidate_team_info(&app, path.team_id).await?;
    db::board::LEADER_BOARD_CACHE
        .invalidate_game(path.game_id)
        .await;
    Ok(HttpResponse::Ok().json(TeamResponse {
        code: TeamAdminResult::Ok,
        team,
    }))
}

async fn add_member(
    path: web::Path<TeamPathInfo>,
    req: web::Json<MemberAddRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::team::admin_add_member(&app, path.game_id, path.team_id, req.user_id).await?;
    match result {
        db::team::AdminMemberResult::Ok => get(path, app).await,
        db::team::AdminMemberResult::Conflict => {
            RbError::conflict(TeamAdminResult::Conflict.into()).http_err()
        }
        db::team::AdminMemberResult::LastMember | db::team::AdminMemberResult::NotFound => {
            RbError::not_found()
                .code(TeamAdminResult::NotFound.into())
                .http_err()
        }
    }
}

async fn remove_member(
    path: web::Path<MemberPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result =
        db::team::admin_remove_member(&app, path.game_id, path.team_id, path.user_id).await?;
    match result {
        db::team::AdminMemberResult::Ok => {
            let team = db::team::admin_get(&app.db, path.game_id, path.team_id)
                .await?
                .ok_or(RbError::not_found())?;
            Ok(HttpResponse::Ok().json(TeamResponse {
                code: TeamAdminResult::Ok,
                team,
            }))
        }
        db::team::AdminMemberResult::LastMember => {
            RbError::conflict(TeamAdminResult::LastMember.into()).http_err()
        }
        db::team::AdminMemberResult::Conflict | db::team::AdminMemberResult::NotFound => {
            RbError::not_found()
                .code(TeamAdminResult::NotFound.into())
                .http_err()
        }
    }
}

async fn promote_member(
    path: web::Path<MemberPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result =
        db::team::admin_promote_member(&app, path.game_id, path.team_id, path.user_id).await?;
    match result {
        db::team::AdminMemberResult::Ok => {
            let team = db::team::admin_get(&app.db, path.game_id, path.team_id)
                .await?
                .ok_or(RbError::not_found())?;
            Ok(HttpResponse::Ok().json(TeamResponse {
                code: TeamAdminResult::Ok,
                team,
            }))
        }
        _ => RbError::not_found()
            .code(TeamAdminResult::NotFound.into())
            .http_err(),
    }
}

async fn update_currency(
    path: web::Path<CurrencyPathInfo>,
    req: web::Json<CurrencyUpdateRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let currency = db::team::update_currency(
        &app.db,
        path.team_id,
        path.currency_id,
        db::team::UpdateCurrencyOptions {
            amount: req.amount,
            growth: req.growth,
            hidden: req.hidden,
        },
    )
    .await?;
    if currency.is_none() {
        return RbError::not_found()
            .code(TeamAdminResult::NotFound.into())
            .http_err();
    }
    let team = db::team::admin_get(&app.db, path.game_id, path.team_id)
        .await?
        .ok_or(RbError::not_found())?;
    Ok(HttpResponse::Ok().json(TeamResponse {
        code: TeamAdminResult::Ok,
        team,
    }))
}

async fn delete(path: web::Path<TeamPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let team = sqlx::query_as!(
        RbTeam,
        "SELECT * FROM rb_team WHERE game_id = $1 AND id = $2;",
        path.game_id,
        path.team_id
    )
    .fetch_optional(&app.db)
    .await
    .map_err(crate::error::RbInternalError::from)?;
    let Some(team) = team else {
        return RbError::not_found()
            .code(TeamAdminResult::NotFound.into())
            .http_err();
    };
    db::team::admin_delete(&app, path.game_id, path.team_id).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "code": TeamAdminResult::Ok,
        "team": team,
    })))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/{game_id}/teams", web::get().to(list))
        .route("/{game_id}/teams", web::post().to(create))
        .route("/{game_id}/teams/users", web::get().to(search_users))
        .route("/{game_id}/teams/{team_id}", web::get().to(get))
        .route("/{game_id}/teams/{team_id}", web::patch().to(update))
        .route("/{game_id}/teams/{team_id}", web::delete().to(delete))
        .route(
            "/{game_id}/teams/{team_id}/members",
            web::post().to(add_member),
        )
        .route(
            "/{game_id}/teams/{team_id}/members/{user_id}",
            web::delete().to(remove_member),
        )
        .route(
            "/{game_id}/teams/{team_id}/members/{user_id}/captain",
            web::post().to(promote_member),
        )
        .route(
            "/{game_id}/teams/{team_id}/currencies/{currency_id}",
            web::patch().to(update_currency),
        );
}

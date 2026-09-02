use std::collections::HashSet;

use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        puzzle::{
            PuzzleSubmitRequirement, RbPuzzleAdminData, RbPuzzleCreateData, RbPuzzleUpdateData,
        },
    },
    error::{RbError, RbInternalError},
    expr,
    model::game::{RbContentType, RbPuzzlePenaltyType, RbPuzzleType},
};

fn is_constraint_error(err: &RbInternalError) -> bool {
    matches!(
        err,
        RbInternalError::Sql(sqlx::Error::Database(db_err))
            if db_err.code().is_some_and(|code| code == "23505" || code == "23514")
    )
}

#[derive(Deserialize)]
struct PuzzlePathInfo {
    puzzle_id: i32,
}

#[derive(Deserialize)]
struct PuzzleListQuery {
    game_id: Option<i32>,
}

#[derive(Deserialize)]
struct PuzzleBatchReleaseRequest {
    game_id: i32,
    puzzle_ids: Vec<i32>,
    release_phase_id: Option<i32>,
    #[serde(default)]
    release_immediately: bool,
}

#[derive(Deserialize)]
struct PenaltyRule {
    #[serde(rename = "type")]
    rtype: RbPuzzlePenaltyType,
    args: Vec<i64>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum PuzzleAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct PuzzleAdminResponse {
    code: PuzzleAdminResult,
    puzzle: RbPuzzleAdminData,
}

#[derive(Serialize)]
struct PuzzleAdminListResponse {
    code: PuzzleAdminResult,
    puzzles: Vec<RbPuzzleAdminData>,
}

#[derive(Serialize)]
struct PuzzleAdminDeleteResponse {
    code: PuzzleAdminResult,
}

#[derive(Serialize)]
struct PuzzleAdminUnlockCheckResponse {
    code: PuzzleAdminResult,
    unlocked: usize,
}

#[derive(Serialize)]
struct PuzzleAdminClearStatesResponse {
    code: PuzzleAdminResult,
    result: db::puzzle::AdminClearPuzzleTeamStatesResult,
    backend_kv: usize,
    backend_store: usize,
    unlocked: usize,
}

#[derive(Deserialize)]
struct PuzzleAdminClearStatesRequest {
    #[serde(default = "default_check_unlock")]
    check_unlock: bool,
}

fn default_check_unlock() -> bool {
    true
}

fn validate_json_shape(data: &RbPuzzleCreateData) -> bool {
    data.judge.is_object() || data.judge.is_array()
}

fn validate_judge_action(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().all(validate_judge_action),
        serde_json::Value::Object(map) => {
            let rule_type = map.get("type").and_then(serde_json::Value::as_str);
            if map
                .get("action")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|action| action == "pending")
            {
                return false;
            }

            if matches!(rule_type, Some("custom"))
                && map
                    .get("function")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|value| value.is_empty())
            {
                return false;
            }

            if map.get("triggers").is_some_and(|value| {
                value.as_array().is_none_or(|items| {
                    items.iter().any(|item| {
                        item.as_str()
                            .is_none_or(|key| !crate::game::judge::valid_trigger_key(key))
                    })
                })
            }) {
                return false;
            }

            map.values().all(validate_judge_action)
        }
        _ => true,
    }
}

fn validate_ptype(value: i16) -> bool {
    matches!(
        RbPuzzleType::from(value),
        RbPuzzleType::Normal | RbPuzzleType::Story
    )
}

fn validate_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

fn validate_unlock_cond(value: Option<&str>) -> bool {
    value.is_none_or(|condition| expr::compile_gate_expr(condition).is_ok())
}

fn validate_slug(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn validate_slug_option(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(validate_slug)
}

fn validate_create(data: &RbPuzzleCreateData) -> bool {
    validate_ptype(data.ptype)
        && validate_content_type(data.content_type)
        && validate_slug_option(&data.slug)
        && data.ticket_cooldown >= 0
        && data.max_submit.is_none_or(|value| value >= 0)
        && validate_unlock_cond(data.unlock_cond.as_deref())
        && validate_json_shape(data)
        && validate_judge_action(&data.judge)
        && data.penalty.is_array()
        && data.submit_requirements.is_array()
        && !(data.release_immediately && data.release_phase_id.is_some())
}

fn validate_update(data: &RbPuzzleUpdateData) -> bool {
    if data.release_immediately == Some(false)
        || (data.release_immediately.is_some() && data.release_phase_id.is_some())
    {
        return false;
    }
    if let Some(ptype) = data.ptype
        && !validate_ptype(ptype)
    {
        return false;
    }

    if let Some(slug) = &data.slug
        && !validate_slug_option(slug)
    {
        return false;
    }

    if let Some(ticket_cooldown) = data.ticket_cooldown
        && ticket_cooldown < 0
    {
        return false;
    }

    if let Some(Some(max_submit)) = data.max_submit
        && max_submit < 0
    {
        return false;
    }

    if let Some(judge) = &data.judge
        && (!(judge.is_object() || judge.is_array()) || !validate_judge_action(judge))
    {
        return false;
    }

    if let Some(penalty) = &data.penalty
        && !penalty.is_array()
    {
        return false;
    }

    if let Some(requirements) = &data.submit_requirements
        && !requirements.is_array()
    {
        return false;
    }

    if let Some(unlock_cond) = &data.unlock_cond
        && !validate_unlock_cond(unlock_cond.as_deref())
    {
        return false;
    }

    true
}

async fn validate_penalty_currency(
    app: &AppState,
    game_id: i32,
    penalty: &serde_json::Value,
) -> Result<bool, RbInternalError> {
    let rules: Vec<PenaltyRule> = match serde_json::from_value(penalty.clone()) {
        Ok(rules) => rules,
        Err(_) => return Ok(false),
    };

    let mut has_cooldown = false;
    for rule in rules {
        match rule.rtype {
            RbPuzzlePenaltyType::FixedTime | RbPuzzlePenaltyType::LinearTime => {
                if has_cooldown {
                    return Ok(false);
                }
                has_cooldown = true;

                if rule.args.first().is_none_or(|value| *value < 0) {
                    return Ok(false);
                }
            }
            RbPuzzlePenaltyType::Currency => {
                let Some(currency_id) = rule.args.first() else {
                    return Ok(false);
                };
                let Some(amount) = rule.args.get(1) else {
                    return Ok(false);
                };
                let Ok(currency_id) = i32::try_from(*currency_id) else {
                    return Ok(false);
                };
                if currency_id <= 0
                    || *amount <= 0
                    || !db::game::currency_belongs_to_game(&app.db, game_id, currency_id).await?
                {
                    return Ok(false);
                }
            }
            RbPuzzlePenaltyType::No => {}
        }
    }

    Ok(true)
}

async fn validate_submit_requirements(
    app: &AppState,
    game_id: i32,
    value: &serde_json::Value,
) -> Result<bool, RbInternalError> {
    let requirements: Vec<PuzzleSubmitRequirement> = match serde_json::from_value(value.clone()) {
        Ok(requirements) => requirements,
        Err(_) => return Ok(false),
    };
    let mut currencies = HashSet::new();
    for requirement in requirements {
        let PuzzleSubmitRequirement::CurrencyMinimum {
            currency_id,
            minimum,
        } = requirement;
        if currency_id <= 0
            || minimum <= 0
            || !currencies.insert(currency_id)
            || !db::game::currency_belongs_to_game(&app.db, game_id, currency_id).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn list(
    query: web::Query<PuzzleListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzles = db::puzzle::admin_list(&app.db, query.game_id).await?;

    Ok(HttpResponse::Ok().json(PuzzleAdminListResponse {
        code: PuzzleAdminResult::Ok,
        puzzles,
    }))
}

async fn get(path: web::Path<PuzzlePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let puzzle = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn append(
    req: web::Json<RbPuzzleCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_create(&req) {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let game_id = db::round::get_round_game(&app.db, req.round_id).await?;
    let Some(game_id) = game_id else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    if !validate_penalty_currency(&app, game_id, &req.penalty).await? {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }
    if !validate_submit_requirements(&app, game_id, &req.submit_requirements).await? {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let puzzle = match db::puzzle::admin_create(&app.db, &req).await {
        Ok(puzzle) => puzzle,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };
    crate::module::release::wake_scheduler(app.get_ref());

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn batch_update_release_phase(
    req: web::Json<PuzzleBatchReleaseRequest>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzle_ids = req.puzzle_ids.iter().copied().collect::<HashSet<_>>();
    if req.game_id <= 0
        || req.release_phase_id.is_some_and(|id| id <= 0)
        || (req.release_immediately && req.release_phase_id.is_some())
        || puzzle_ids.is_empty()
        || puzzle_ids.len() > 500
        || puzzle_ids.iter().any(|id| *id <= 0)
    {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let mut puzzle_ids = puzzle_ids.into_iter().collect::<Vec<_>>();
    puzzle_ids.sort_unstable();
    let Some(puzzles) = db::puzzle::admin_batch_update_release_phase(
        &app.db,
        req.game_id,
        &puzzle_ids,
        req.release_phase_id,
        req.release_immediately,
    )
    .await?
    else {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    };

    crate::module::release::wake_scheduler(app.get_ref());

    Ok(HttpResponse::Ok().json(PuzzleAdminListResponse {
        code: PuzzleAdminResult::Ok,
        puzzles,
    }))
}

async fn edit(
    path: web::Path<PuzzlePathInfo>,
    req: web::Json<RbPuzzleUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_update(&req) {
        return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
    }

    let previous = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(previous_data) = previous.as_ref() else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    if req.penalty.is_some() || req.submit_requirements.is_some() || req.round_id.is_some() {
        let game_id = if let Some(round_id) = req.round_id {
            let game_id = db::round::get_round_game(&app.db, round_id).await?;
            let Some(game_id) = game_id else {
                return RbError::not_found()
                    .code(PuzzleAdminResult::NotFound.into())
                    .http_err();
            };
            game_id
        } else {
            previous_data.game_id
        };
        let penalty = req.penalty.as_ref().unwrap_or(&previous_data.penalty);

        if !validate_penalty_currency(&app, game_id, penalty).await? {
            return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
        }
        let requirements = req
            .submit_requirements
            .as_ref()
            .unwrap_or(&previous_data.submit_requirements);
        if !validate_submit_requirements(&app, game_id, requirements).await? {
            return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
        }
    }

    let puzzle = match db::puzzle::admin_update(&app.db, path.puzzle_id, &req).await {
        Ok(puzzle) => puzzle,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(PuzzleAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };
    crate::module::release::wake_scheduler(app.get_ref());

    Ok(HttpResponse::Ok().json(PuzzleAdminResponse {
        code: PuzzleAdminResult::Ok,
        puzzle,
    }))
}

async fn delete(path: web::Path<PuzzlePathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let deleted = db::puzzle::admin_delete(&app.db, path.puzzle_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    }
    Ok(HttpResponse::Ok().json(PuzzleAdminDeleteResponse {
        code: PuzzleAdminResult::Ok,
    }))
}

async fn unlock_check(
    path: web::Path<PuzzlePathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzle = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    let unlocked_team_ids = db::puzzle::admin_unlock_puzzle_for_eligible_teams(
        &app,
        puzzle.id,
        puzzle.game_id,
        puzzle.unlock_cond.as_deref(),
    )
    .await?;

    Ok(HttpResponse::Ok().json(PuzzleAdminUnlockCheckResponse {
        code: PuzzleAdminResult::Ok,
        unlocked: unlocked_team_ids.len(),
    }))
}

async fn clear_states(
    path: web::Path<PuzzlePathInfo>,
    req: Option<web::Json<PuzzleAdminClearStatesRequest>>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let puzzle = db::puzzle::admin_get(&app.db, path.puzzle_id).await?;
    let Some(puzzle) = puzzle else {
        return RbError::not_found()
            .code(PuzzleAdminResult::NotFound.into())
            .http_err();
    };

    let mut result = db::puzzle::admin_clear_puzzle_team_states(&app.db, puzzle.id).await?;
    let backend_kv = db::puzzle_backend::clear_puzzle_team_kv(&app.db, puzzle.id).await?;
    let backend_store = db::puzzle_backend::clear_puzzle_team_store(&app.db, puzzle.id).await?;
    let backend_kv_count = backend_kv.rows;
    let backend_store_count = backend_store.rows;
    result.team_ids.extend(backend_kv.team_ids);
    result.team_ids.extend(backend_store.team_ids);
    result.team_ids.sort_unstable();
    result.team_ids.dedup();
    result.team_count = result.team_ids.len();
    let check_unlock = req.as_ref().is_none_or(|req| req.check_unlock);
    let unlocked_team_ids = if check_unlock {
        db::puzzle::admin_unlock_puzzle_for_eligible_teams(
            &app,
            puzzle.id,
            puzzle.game_id,
            puzzle.unlock_cond.as_deref(),
        )
        .await?
    } else {
        Vec::new()
    };

    if !result.team_ids.is_empty() {
        let _ = db::cache::del_pattern(
            &app.kv,
            &format!("cache:puzzle-hints:v1:puzzle:{}:team:*", puzzle.id),
        )
        .await;

        for team_id in &result.team_ids {
            db::board::LEADER_BOARD_CACHE
                .update_team(&app.db, *team_id, true)
                .await?;
        }
    }

    Ok(HttpResponse::Ok().json(PuzzleAdminClearStatesResponse {
        code: PuzzleAdminResult::Ok,
        result,
        backend_kv: backend_kv_count,
        backend_store: backend_store_count,
        unlocked: unlocked_team_ids.len(),
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("puzzles")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route(
                "/batch/release-phase",
                web::patch().to(batch_update_release_phase),
            )
            .route("/{puzzle_id}", web::get().to(get))
            .route("/{puzzle_id}", web::patch().to(edit))
            .route("/{puzzle_id}/unlock-check", web::post().to(unlock_check))
            .route("/{puzzle_id}/clear-states", web::post().to(clear_states))
            .route("/{puzzle_id}", web::delete().to(delete)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_immediate_release_patch_shape() {
        assert!(validate_update(&RbPuzzleUpdateData {
            release_immediately: Some(true),
            ..Default::default()
        }));
        assert!(!validate_update(&RbPuzzleUpdateData {
            release_immediately: Some(false),
            ..Default::default()
        }));
        assert!(!validate_update(&RbPuzzleUpdateData {
            release_phase_id: Some(None),
            release_immediately: Some(true),
            ..Default::default()
        }));
        assert!(validate_update(&RbPuzzleUpdateData {
            submit_requirements: Some(serde_json::json!([])),
            ..Default::default()
        }));
        assert!(!validate_update(&RbPuzzleUpdateData {
            submit_requirements: Some(serde_json::json!({})),
            ..Default::default()
        }));
    }
}

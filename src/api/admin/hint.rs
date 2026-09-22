use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        puzzle::{RbHintAdminData, RbHintCreateData, RbHintUpdateData},
    },
    error::{RbError, RbInternalError},
    expr,
    model::game::RbContentType,
};

fn is_constraint_error(err: &RbInternalError) -> bool {
    matches!(
        err,
        RbInternalError::Sql(sqlx::Error::Database(db_err))
            if db_err.code().is_some_and(|code| code == "23503" || code == "23514")
    )
}

#[derive(Deserialize)]
struct HintPathInfo {
    hint_id: i32,
}

#[derive(Deserialize)]
struct HintListQuery {
    puzzle_id: Option<i32>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum HintAdminResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct HintAdminResponse {
    code: HintAdminResult,
    hint: RbHintAdminData,
}

#[derive(Serialize)]
struct HintAdminListResponse {
    code: HintAdminResult,
    hints: Vec<RbHintAdminData>,
}

#[derive(Serialize)]
struct HintAdminDeleteResponse {
    code: HintAdminResult,
}

fn validate_content_type(value: i16) -> bool {
    matches!(
        RbContentType::from(value),
        RbContentType::Markdown | RbContentType::Html | RbContentType::UnsafeMarkdown
    )
}

struct HintBasicValidation<'a> {
    title: Option<&'a str>,
    hidden_title: Option<&'a str>,
    content_type: Option<i16>,
    cooldown: Option<i32>,
    cooldown_origin: Option<i16>,
    title_display_condition: Option<&'a str>,
    display_condition: Option<&'a str>,
    cost_amount: Option<i64>,
    backend_function: Option<&'a str>,
    triggers: Option<&'a [String]>,
}

fn validate_title(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 120
}

fn validate_basic(data: HintBasicValidation<'_>) -> bool {
    data.title.is_none_or(validate_title)
        && data.hidden_title.is_none_or(validate_title)
        && data.content_type.is_none_or(validate_content_type)
        && data.cooldown.is_none_or(|value| value >= 0)
        && data
            .cooldown_origin
            .is_none_or(|value| (0..=3).contains(&value))
        && data
            .title_display_condition
            .is_none_or(valid_display_condition)
        && data.display_condition.is_none_or(valid_display_condition)
        && data.cost_amount.is_none_or(|value| value >= 0)
        && data.backend_function.is_none_or(validate_backend_function)
        && data.triggers.is_none_or(validate_triggers)
}

fn valid_display_condition(value: &str) -> bool {
    expr::compile_hint_display_expr(value).is_ok()
}

fn display_condition_uses_cooldown(value: Option<&str>) -> bool {
    value
        .and_then(|condition| expr::compile_hint_display_expr(condition).ok())
        .is_some_and(|condition| expr::ast::hint_display_uses_cooldown(&condition))
}

fn validate_triggers(values: &[String]) -> bool {
    values
        .iter()
        .all(|value| crate::game::judge::valid_trigger_key(value))
}

fn validate_backend_function(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && value.len() <= 64
}

fn validate_enable_condition(value: Option<&str>) -> bool {
    value.is_none_or(|condition| expr::compile_gate_expr(condition).is_ok())
}

async fn get_hint_game(app: &AppState, puzzle_id: i32) -> Result<Option<i32>, RbInternalError> {
    db::puzzle::get_puzzle_game(&app.db, puzzle_id).await
}

async fn validate_create(app: &AppState, data: &RbHintCreateData) -> Result<bool, RbInternalError> {
    if !validate_basic(HintBasicValidation {
        title: Some(&data.title),
        hidden_title: data.hidden_title.as_deref(),
        content_type: Some(data.content_type),
        cooldown: Some(data.cooldown),
        cooldown_origin: Some(data.cooldown_origin),
        title_display_condition: data.title_display_condition.as_deref(),
        display_condition: data.display_condition.as_deref(),
        cost_amount: Some(data.cost_amount),
        backend_function: data.backend_function.as_deref(),
        triggers: Some(&data.triggers),
    }) {
        return Ok(false);
    }
    if !validate_enable_condition(data.enable_cond.as_deref())
        || (data.enable_cond.is_none() && data.cooldown_origin == 1)
        || (data.display_condition.is_none() && data.cooldown_origin == 2)
        || (data.title_display_condition.is_none() && data.cooldown_origin == 3)
        || (data.cooldown_origin == 2
            && display_condition_uses_cooldown(data.display_condition.as_deref()))
        || (data.cooldown_origin == 3
            && display_condition_uses_cooldown(data.title_display_condition.as_deref()))
    {
        return Ok(false);
    }

    let Some(game_id) = get_hint_game(app, data.puzzle_id).await? else {
        return Ok(false);
    };

    if let Some(cost_id) = data.cost_id {
        if cost_id <= 0 || !db::game::currency_belongs_to_game(&app.db, game_id, cost_id).await? {
            return Ok(false);
        }
    } else if data.cost_amount != 0 {
        return Ok(false);
    }

    Ok(true)
}

async fn validate_update(
    app: &AppState,
    current: &RbHintAdminData,
    data: &RbHintUpdateData,
) -> Result<bool, RbInternalError> {
    if !validate_basic(HintBasicValidation {
        title: data.title.as_deref(),
        hidden_title: data
            .hidden_title
            .as_ref()
            .and_then(|title| title.as_deref()),
        content_type: data.content_type,
        cooldown: data.cooldown,
        cooldown_origin: data.cooldown_origin,
        title_display_condition: data
            .title_display_condition
            .as_ref()
            .and_then(|condition| condition.as_deref()),
        display_condition: data
            .display_condition
            .as_ref()
            .and_then(|condition| condition.as_deref()),
        cost_amount: data.cost_amount,
        backend_function: data
            .backend_function
            .as_ref()
            .map(|value| value.as_deref())
            .unwrap_or(current.backend_function.as_deref()),
        triggers: data.triggers.as_deref(),
    }) {
        return Ok(false);
    }

    let enable_cond = data
        .enable_cond
        .as_ref()
        .map(|condition| condition.as_deref())
        .unwrap_or(current.enable_cond.as_deref());
    let cooldown_origin = if matches!(data.enable_cond, Some(None))
        && data.cooldown_origin.unwrap_or(current.cooldown_origin) == 1
    {
        0
    } else {
        data.cooldown_origin.unwrap_or(current.cooldown_origin)
    };
    let title_display_condition = data
        .title_display_condition
        .as_ref()
        .map(|condition| condition.as_deref())
        .unwrap_or(current.title_display_condition.as_deref());
    let display_condition = data
        .display_condition
        .as_ref()
        .map(|condition| condition.as_deref())
        .unwrap_or(current.display_condition.as_deref());
    if !validate_enable_condition(enable_cond)
        || (enable_cond.is_none() && cooldown_origin == 1)
        || (display_condition.is_none() && cooldown_origin == 2)
        || (title_display_condition.is_none() && cooldown_origin == 3)
        || (cooldown_origin == 2 && display_condition_uses_cooldown(display_condition))
        || (cooldown_origin == 3 && display_condition_uses_cooldown(title_display_condition))
    {
        return Ok(false);
    }

    let puzzle_id = data.puzzle_id.unwrap_or(current.puzzle_id);
    let Some(game_id) = get_hint_game(app, puzzle_id).await? else {
        return Ok(false);
    };

    let cost_id = data.cost_id.unwrap_or(current.cost_id);
    if let Some(cost_id) = cost_id
        && (cost_id <= 0 || !db::game::currency_belongs_to_game(&app.db, game_id, cost_id).await?)
    {
        return Ok(false);
    }

    Ok(true)
}

async fn invalidate_hint_cache(app: &AppState, puzzle_id: i32) {
    let _ = db::cache::del_pattern(
        &app.kv,
        &format!("cache:puzzle-hints:v1:puzzle:{puzzle_id}:team:*"),
    )
    .await;
}

async fn list(query: web::Query<HintListQuery>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hints = db::puzzle::admin_list_hints(&app.db, query.puzzle_id).await?;

    Ok(HttpResponse::Ok().json(HintAdminListResponse {
        code: HintAdminResult::Ok,
        hints,
    }))
}

async fn get(path: web::Path<HintPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hint = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn append(
    req: web::Json<RbHintCreateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !validate_create(&app, &req).await? {
        return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
    }

    let hint = match db::puzzle::admin_create_hint(&app.db, &req).await {
        Ok(hint) => hint,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_hint_cache(&app, hint.puzzle_id).await;

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn edit(
    path: web::Path<HintPathInfo>,
    req: web::Json<RbHintUpdateData>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let current = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(current) = current else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    if !validate_update(&app, &current, &req).await? {
        return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
    }

    let hint = match db::puzzle::admin_update_hint(&app.db, path.hint_id, &req).await {
        Ok(hint) => hint,
        Err(err) => {
            if is_constraint_error(&err) {
                return RbError::bad_req(HintAdminResult::Invalid.into()).http_err();
            }
            return Err(err.into());
        }
    };
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };
    invalidate_hint_cache(&app, current.puzzle_id).await;
    if hint.puzzle_id != current.puzzle_id {
        invalidate_hint_cache(&app, hint.puzzle_id).await;
    }

    Ok(HttpResponse::Ok().json(HintAdminResponse {
        code: HintAdminResult::Ok,
        hint,
    }))
}

async fn delete(path: web::Path<HintPathInfo>, app: web::Data<AppState>) -> Result<HttpResponse> {
    let hint = db::puzzle::admin_get_hint(&app.db, path.hint_id).await?;
    let Some(hint) = hint else {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    };

    let deleted = db::puzzle::admin_delete_hint(&app.db, path.hint_id).await?;
    if !deleted {
        return RbError::not_found()
            .code(HintAdminResult::NotFound.into())
            .http_err();
    }
    invalidate_hint_cache(&app, hint.puzzle_id).await;

    Ok(HttpResponse::Ok().json(HintAdminDeleteResponse {
        code: HintAdminResult::Ok,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("hints")
            .route("", web::get().to(list))
            .route("", web::post().to(append))
            .route("/{hint_id}", web::get().to(get))
            .route("/{hint_id}", web::patch().to(edit))
            .route("/{hint_id}", web::delete().to(delete)),
    );
}

#[cfg(test)]
mod tests {
    use super::{valid_display_condition, validate_title, validate_triggers};

    #[test]
    fn hint_display_conditions_accept_hint_and_game_state() {
        assert!(valid_display_condition(
            "(or (hint-enabled) (and (hint-cooled-down) (solved intro)))"
        ));
        assert!(!valid_display_condition("(hint-enabled unexpected)"));
        assert!(!valid_display_condition(""));
    }

    #[test]
    fn hint_triggers_use_gate_trigger_key_rules() {
        assert!(validate_triggers(&[
            "hintUnlocked".to_string(),
            "extra-content_2".to_string(),
        ]));
        assert!(!validate_triggers(&["2-invalid".to_string()]));
        assert!(!validate_triggers(&["contains space".to_string()]));
        assert!(!validate_triggers(&["a".repeat(65)]));
    }

    #[test]
    fn hint_titles_must_be_nonempty_and_at_most_120_characters() {
        assert!(validate_title("Hidden hint"));
        assert!(validate_title(&"a".repeat(120)));
        assert!(!validate_title("   "));
        assert!(!validate_title(&"a".repeat(121)));
    }
}

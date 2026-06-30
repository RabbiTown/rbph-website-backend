use actix_web::{HttpResponse, Result, web};
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    db::{
        self,
        feature::{FeatureChangeData, GameFeature, GameFeatureState},
    },
    error::RbError,
    extractor::auth::AuthUser,
};

#[derive(Deserialize)]
struct GamePath {
    game_id: i32,
}

#[derive(Deserialize)]
struct FeaturePath {
    game_id: i32,
    feature: String,
}

#[derive(Deserialize)]
struct FeatureUpdateRequest {
    state: GameFeatureState,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FeatureResult {
    Invalid = -2,
    NotFound = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct FeatureListResponse {
    code: FeatureResult,
    features: Vec<db::feature::AdminFeatureData>,
}

fn parse_feature(value: &str) -> Option<GameFeature> {
    match value {
        "team_formation" => Some(GameFeature::TeamFormation),
        "direct_message" => Some(GameFeature::DirectMessage),
        "puzzle_ticket" => Some(GameFeature::PuzzleTicket),
        "leaderboard" => Some(GameFeature::Leaderboard),
        _ => None,
    }
}

async fn list(path: web::Path<GamePath>, app: web::Data<AppState>) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    Ok(HttpResponse::Ok().json(FeatureListResponse {
        code: FeatureResult::Ok,
        features: db::feature::list_admin(&app.db, path.game_id).await?,
    }))
}

async fn update(
    path: web::Path<FeaturePath>,
    body: web::Json<FeatureUpdateRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let Some(feature) = parse_feature(&path.feature) else {
        return RbError::bad_req(FeatureResult::Invalid.into()).http_err();
    };
    let change = FeatureChangeData {
        feature,
        state: body.state,
    };
    if !db::feature::valid_changes(std::slice::from_ref(&change)) {
        return RbError::bad_req(FeatureResult::Invalid.into()).http_err();
    }
    let Some(leaderboard_changed) =
        db::feature::set_manual_state(&app.db, path.game_id, &change, user.uid).await?
    else {
        return RbError::not_found()
            .code(FeatureResult::NotFound.into())
            .http_err();
    };
    if leaderboard_changed {
        db::board::LEADER_BOARD_CACHE
            .invalidate_game(path.game_id)
            .await;
    }
    app.sync_hub.notify_game_release_updated(
        path.game_id,
        db::release::release_cursor(&app.db, path.game_id).await?,
    );
    Ok(HttpResponse::Ok().json(FeatureListResponse {
        code: FeatureResult::Ok,
        features: db::feature::list_admin(&app.db, path.game_id).await?,
    }))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{game_id}/features")
            .route("", web::get().to(list))
            .route("/{feature}", web::patch().to(update)),
    );
}

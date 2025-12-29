use actix_web::{HttpResponse, Result, web};

use crate::{AppState, db, extractor::auth::AuthUser};

pub async fn hello() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().body("wowwo so privleged"))
}

pub async fn info(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::user::get_display_by_id(&app.db, user.uid).await?;

    Ok(HttpResponse::Ok().json(result))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/hello", web::get().to(hello))
        .route("/info", web::get().to(info));
}

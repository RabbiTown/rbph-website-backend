use actix_web::{HttpResponse, Result, web};
use serde::Deserialize;

#[derive(Deserialize)]
struct PathInfo {
    _game_id: i32,
}

async fn append() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

async fn edit(_path: web::Path<PathInfo>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().finish())
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(append))
        .route("/{game_id}", web::patch().to(edit));
}

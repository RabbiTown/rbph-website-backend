use actix_web::{HttpResponse, Result, web};

pub async fn hello() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().body("wowwo so privleged"))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("hello", web::get().to(hello));
}

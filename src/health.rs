use actix_web::{HttpResponse, web};
use deadpool_redis::redis;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn live() -> HttpResponse {
    HttpResponse::Ok().json(HealthResponse { status: "ok" })
}

async fn ready(app: web::Data<AppState>) -> HttpResponse {
    let db_ready = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&app.db)
        .await
        .is_ok();
    let kv_ready = match app.kv.get().await {
        Ok(mut connection) => redis::cmd("PING")
            .query_async::<String>(&mut connection)
            .await
            .is_ok(),
        Err(_) => false,
    };
    let storage_ready = tokio::fs::metadata(app.storage.root())
        .await
        .is_ok_and(|metadata| metadata.is_dir());

    if db_ready && kv_ready && storage_ready {
        HttpResponse::Ok().json(HealthResponse { status: "ready" })
    } else {
        HttpResponse::ServiceUnavailable().json(HealthResponse {
            status: "not_ready",
        })
    }
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/health/live", web::get().to(live))
        .route("/health/ready", web::get().to(ready));
}

use actix_files::NamedFile;
use actix_web::{HttpRequest, HttpResponse, Result, web};

use crate::{AppState, db};

#[derive(serde::Deserialize)]
struct AssetPathInfo {
    object_key: String,
    filename: String,
}

async fn get(
    request: HttpRequest,
    path: web::Path<AssetPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let backend = db::asset::get_public_file_backend(&app.db, &path.object_key, &path.filename)
        .await?
        .ok_or_else(crate::error::RbError::not_found)?;

    if let Some(local) = app.storage.local(&backend) {
        let file_path = local.object_path(&path.object_key, &path.filename);
        return Ok(NamedFile::open_async(file_path)
            .await?
            .into_response(&request));
    }

    let url = app
        .storage
        .public_url(&backend, &path.object_key, &path.filename)
        .ok_or_else(crate::error::RbError::not_found)?;
    Ok(HttpResponse::Found()
        .insert_header((actix_web::http::header::LOCATION, url))
        .finish())
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/assets/{object_key}/{filename:.*}", web::get().to(get));
}

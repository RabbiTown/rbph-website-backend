use actix_files::NamedFile;
use actix_web::{Result, web};

use crate::AppState;

#[derive(serde::Deserialize)]
struct AssetPathInfo {
    object_key: String,
    filename: String,
}

async fn get(path: web::Path<AssetPathInfo>, app: web::Data<AppState>) -> Result<NamedFile> {
    let file_path = app.storage.object_path(&path.object_key, &path.filename);
    Ok(NamedFile::open_async(file_path).await?)
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("/assets/{object_key}/{filename:.*}", web::get().to(get));
}

use actix_web::{
    HttpRequest, HttpResponse, Result,
    web::{self, Payload},
};

use crate::{AppState, extractor::auth::AuthUser};

async fn sync_ws(
    req: HttpRequest,
    stream: Payload,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    app.sync_hub.create_ws(req, stream, user.uid)
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(sync_ws));
}

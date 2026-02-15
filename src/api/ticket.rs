use actix::fut::{Ready, ready};
use actix_session::SessionExt;
use actix_web::{
    Error, FromRequest, HttpMessage, HttpRequest, HttpResponse, Result,
    body::MessageBody,
    dev::{Payload, ServiceRequest, ServiceResponse},
    middleware::{self, Next},
    web::{self},
};
use num_enum::IntoPrimitive;
use serde::Deserialize;
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    api::error_handler,
    db::{self, ticket::TicketUserInfo},
    error::RbError,
    extractor::auth::AuthUser,
    model::game::{RbContentType, RbTicketSenderType},
};

#[derive(Deserialize)]
struct TicketPathInfo {
    ticket_id: i32,
}

impl FromRequest for TicketUserInfo {
    type Error = Error;
    type Future = Ready<Result<Self, Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        match req.extensions().get::<TicketUserInfo>().cloned() {
            Some(x) => ready(Ok(x)),
            None => ready(Err(RbError::not_found().into())),
        }
    }
}

// -- get --

async fn get_ticket(
    path: web::Path<TicketPathInfo>,
    info: TicketUserInfo,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result =
        db::ticket::get_ticket_aggre_info(&app.db, path.ticket_id, !info.mod_access).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_dm_ticket(
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::ticket::get_dm_ticket_aggre_info(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        !info.mod_access,
    )
    .await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

// -- send --

fn def_content_type() -> RbContentType {
    RbContentType::UnsafeMarkdown
}

#[derive(Deserialize)]
struct TicketSendRequest {
    content: String,
    #[serde(default = "def_content_type")]
    content_type: RbContentType,
    sender_type: RbTicketSenderType,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketSendResult {
    ContentTooLong = -2,
    BadContentType = -1,
    Ok = 0,
}

async fn send_ticket_message(
    req: web::Json<TicketSendRequest>,
    path: web::Path<TicketPathInfo>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !info.member_access {
        RbError::forbid().err()?
    }

    if !user.req_role()?.is_admin() {
        if req.content_type.is_trusted() {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
        }
        // TODO: make limit configurable
        if req.content.len() > 1000 {
            RbError::unprocessable(TicketSendResult::ContentTooLong.into()).err()?
        }
    }

    Ok(HttpResponse::Ok().finish())
}

/// check if user has any accessibility to the ticket
async fn check_ticket_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let ticket_id: i32 = req
        .match_info()
        .get("ticket_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;

    let user_id: i32 = req
        .get_session()
        .get::<i32>("user_id")
        .ok()
        .flatten()
        .ok_or_else(RbError::not_found)?;

    let app = req.app_data::<web::Data<AppState>>().unwrap();

    let info = db::ticket::get_ticket_user_info(&app.db, ticket_id, user_id)
        .await?
        .filter(|info| info.any_access())
        .ok_or_else(RbError::not_found)?;

    req.extensions_mut().insert(info);

    next.call(req).await
}

// /games/{game_id}/tickets/self  - for DM ticket
// /games/{game_id}/tickets - list all tickets, for mod only

pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("self")
            .route("", web::get().to(get_dm_ticket))
            .default_service(web::route().to(error_handler)),
    );
}

// /puzzles/{puzzle_id}/tickets - get tickets

// /tickets/...
pub fn tickets_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{ticket_id}")
            .wrap(middleware::from_fn(check_ticket_middleware))
            .route("", web::get().to(get_ticket))
            .route("send", web::post().to(send_ticket_message))
            .default_service(web::route().to(error_handler)),
    );
}

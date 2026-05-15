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
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;

use crate::{
    AppState,
    api::{error_handler, puzzle::PuzzlePathInfo},
    db::{
        self,
        ticket::{SendMessageData, TicketMessage, TicketSummary, TicketThread, TicketUserInfo},
    },
    error::RbError,
    extractor::auth::AuthUser,
    model::game::{RbContentType, RbTicketSenderType, RbTicketState},
};

#[derive(Deserialize)]
struct TicketPathInfo {
    ticket_id: i32,
}

#[derive(Deserialize)]
struct TicketMessagePathInfo {
    ticket_id: i32,
    message_id: i32,
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
    let result = db::ticket::get_ticket_thread(&app.db, path.ticket_id, &info).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_dm_ticket(user: AuthUser, app: web::Data<AppState>) -> Result<HttpResponse> {
    let result = db::ticket::get_dm_ticket_thread(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        user.req_role()?.is_moderator(),
        user.req_role()?.is_admin(),
    )
    .await?;

    Ok(HttpResponse::Ok().json(result))
}

// -- send --

fn def_content_type() -> RbContentType {
    RbContentType::UnsafeMarkdown
}

fn def_sender_type() -> RbTicketSenderType {
    RbTicketSenderType::Team
}

#[derive(Deserialize)]
struct TicketSendRequest {
    content: String,
    #[serde(default = "def_content_type")]
    content_type: RbContentType,
    #[serde(default = "def_sender_type")]
    sender_type: RbTicketSenderType,
    #[serde(default)]
    cost_id: Option<i32>,
    #[serde(default)]
    cost_amount: i32,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketSendResult {
    BadCost = -5,
    ContentTooLong = -4,
    BadContentType = -3,
    PendingExists = -2,
    Closed = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TicketSendResponse {
    code: TicketSendResult,
    message_id: i32,
    ticket: Option<TicketSummary>,
    msg: TicketMessage,
    perm: db::ticket::TicketPerm,
}

async fn do_send_ticket_message(
    req: TicketSendRequest,
    info: &TicketUserInfo,
    user: &AuthUser,
    app: &AppState,
    max_pending: Option<i64>,
) -> Result<HttpResponse> {
    let accessible = match req.sender_type {
        RbTicketSenderType::Team => info.member_access,
        RbTicketSenderType::Host => info.mod_access,
        _ => false,
    };
    if !accessible {
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
        if matches!(info.state, RbTicketState::Closed)
            && matches!(req.sender_type, RbTicketSenderType::Team)
        {
            RbError::forbid()
                .code(TicketSendResult::Closed.into())
                .err()?
        }
    }

    if req.cost_id.is_some() && !matches!(req.sender_type, RbTicketSenderType::Host) {
        RbError::unprocessable(TicketSendResult::BadCost.into()).err()?
    }

    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type: req.sender_type,
        sender_id: user.uid,
        cost_id: req.cost_id,
        cost_amount: req.cost_amount,
    };

    let msg = db::ticket::send_ticket_message(&app.db, info.ticket_id, &data, max_pending).await?;

    if msg.is_none() {
        RbError::conflict(TicketSendResult::PendingExists.into()).err()?
    }
    let msg = msg.unwrap();
    let ticket = db::ticket::get_ticket_summary(&app.db, info.ticket_id, info.mod_access).await?;
    let send_block = db::ticket::calc_send_block(
        &app.db,
        info.ticket_id,
        info.state,
        info.member_access,
        max_pending,
    )
    .await?;
    let currency =
        db::ticket::get_ticket_perm_currency(&app.db, info.ticket_id, info.mod_access).await?;
    let perm = db::ticket::TicketPerm::new(
        info.mod_access,
        info.mod_access,
        info.admin_access,
        currency,
        send_block,
    );

    Ok(HttpResponse::Ok().json(TicketSendResponse {
        code: TicketSendResult::Ok,
        message_id: msg.id,
        ticket,
        msg,
        perm,
    }))
}

async fn send_ticket_message(
    req: web::Json<TicketSendRequest>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    do_send_ticket_message(req.into_inner(), &info, &user, &app, Some(1)).await
}

async fn send_dm_ticket_message(
    req: web::Json<TicketSendRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let ticket_id = db::ticket::get_or_create_dm_ticket_id(
        &app.db,
        user.req_team_id()?.ok_or(RbError::forbid())?,
        user.uid,
    )
    .await?;

    let info = db::ticket::get_ticket_user_info(&app.db, ticket_id, user.uid)
        .await?
        .ok_or(RbError::internal("Invalid ticket id"))?;

    do_send_ticket_message(req.into_inner(), &info, &user, &app, Some(3)).await
}

// -- close --

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketCloseResult {
    Closed = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TicketCloseResponse {
    code: TicketCloseResult,
    ticket: Option<TicketSummary>,
    thread: TicketThread,
    perm: db::ticket::TicketPerm,
}

async fn close_ticket(
    path: web::Path<TicketPathInfo>,
    req: Option<web::Json<TicketSendRequest>>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !info.mod_access {
        RbError::forbid().err()?
    }

    let message = req
        .map(web::Json::into_inner)
        .filter(|req| !req.content.is_empty());

    if let Some(message) = message.as_ref() {
        if !matches!(message.sender_type, RbTicketSenderType::Host) {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
        }
        if !user.req_role()?.is_admin() {
            if message.content_type.is_trusted() {
                RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
            }
            if message.content.len() > 1000 {
                RbError::unprocessable(TicketSendResult::ContentTooLong.into()).err()?
            }
        }
        if message.cost_id.is_some() {
            RbError::unprocessable(TicketSendResult::BadCost.into()).err()?
        }
    }

    let message = message.map(|message| SendMessageData {
        content: message.content,
        content_type: message.content_type,
        sender_type: RbTicketSenderType::Host,
        sender_id: user.uid,
        cost_id: None,
        cost_amount: 0,
    });

    let updated = db::ticket::close_ticket(
        &app.db,
        path.ticket_id,
        user.uid,
        RbTicketSenderType::Host,
        message.as_ref(),
    )
    .await?;
    if !updated {
        RbError::conflict(TicketCloseResult::Closed.into()).err()?
    }

    let ticket = db::ticket::get_ticket_summary(&app.db, path.ticket_id, info.mod_access).await?;
    let refreshed_info = db::ticket::get_ticket_user_info(&app.db, path.ticket_id, user.uid)
        .await?
        .ok_or(RbError::internal("Invalid ticket id"))?;
    let thread = db::ticket::get_ticket_thread(&app.db, path.ticket_id, &refreshed_info)
        .await?
        .ok_or(RbError::internal("Closed ticket not found"))?;
    let perm = thread.perm().clone();

    Ok(HttpResponse::Ok().json(TicketCloseResponse {
        code: TicketCloseResult::Ok,
        ticket,
        thread,
        perm,
    }))
}

// -- purchase message --

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketMessagePurchaseResult {
    Insufficient = -2,
    Unavailable = -1,
    Ok = 0,
}

async fn purchase_ticket_message(
    path: web::Path<TicketMessagePathInfo>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !info.member_access && !info.mod_access {
        RbError::forbid().err()?
    }

    let purchase_result =
        db::ticket::purchase_ticket_message(&app, &user, user.uid, path.ticket_id, path.message_id)
            .await?;

    match purchase_result {
        db::ticket::PurchaseTicketMessageResult::Unavailable => {
            RbError::conflict(TicketMessagePurchaseResult::Unavailable.into()).http_err()
        }
        db::ticket::PurchaseTicketMessageResult::Insufficient => {
            RbError::conflict(TicketMessagePurchaseResult::Insufficient.into()).http_err()
        }
        db::ticket::PurchaseTicketMessageResult::Ok(message) => {
            Ok(HttpResponse::Ok().json(message))
        }
    }
}

// -- open --

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketOpenResult {
    ContentTooLong = -5,
    BadContentType = -4,
    Cooldown = -3,
    PendingExists = -2,
    Invalid = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct TicketOpenResponse {
    code: TicketOpenResult,
    ticket_id: i32,
    message_id: i32,
    thread: TicketThread,
}

async fn open_ticket(
    req: web::Json<TicketSendRequest>,
    path: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;

    if !matches!(req.sender_type, RbTicketSenderType::Team) || req.cost_id.is_some() {
        RbError::unprocessable(TicketOpenResult::Invalid.into()).err()?
    }

    if !user.req_role()?.is_admin() {
        if req.content_type.is_trusted() {
            RbError::unprocessable(TicketOpenResult::BadContentType.into()).err()?
        }
        // TODO: make limit configurable
        if req.content.len() > 1000 {
            RbError::unprocessable(TicketOpenResult::ContentTooLong.into()).err()?
        }
    }

    let req = req.into_inner();
    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type: RbTicketSenderType::Team,
        sender_id: user.uid,
        cost_id: None,
        cost_amount: 0,
    };

    let result = db::ticket::open_puzzle_ticket(&app.db, team_id, path.puzzle_id, &data).await?;

    match result {
        db::ticket::OpenPuzzleTicketResult::PendingExists => {
            RbError::conflict(TicketOpenResult::PendingExists.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::Cooldown => {
            RbError::conflict(TicketOpenResult::Cooldown.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::Ok(thread) => {
            let thread = *thread;
            let ticket = thread
                .ticket()
                .ok_or_else(|| RbError::internal("Opened ticket not found"))?;
            let msg = thread
                .messages()
                .iter()
                .find_map(|item| item.message_id())
                .ok_or_else(|| RbError::internal("Opened ticket message not found"))?;

            Ok(HttpResponse::Ok().json(TicketOpenResponse {
                code: TicketOpenResult::Ok,
                ticket_id: ticket.id(),
                message_id: msg,
                thread,
            }))
        }
    }
}

// -- puzzle: get list --

async fn get_team_puzzle_tickets(
    path: web::Path<PuzzlePathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let result = db::ticket::get_team_puzzle_tickets(&app.db, team_id, path.puzzle_id).await?;

    Ok(HttpResponse::Ok().json(result))
}

/// Check if user has any accessibility to the ticket.
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
        .filter(|info| (info.member_access && info.puzzle_id.is_some()) || info.mod_access)
        .ok_or_else(RbError::not_found)?;

    req.extensions_mut().insert(info);

    next.call(req).await
}

pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("self")
            .route("", web::get().to(get_dm_ticket))
            .route("/send", web::post().to(send_dm_ticket_message))
            .default_service(web::route().to(error_handler)),
    );
}

// /puzzles/{puzzle_id}/tickets - get/open tickets

pub fn puzzles_config(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::get().to(get_team_puzzle_tickets))
        .route("", web::post().to(open_ticket));
}

// /tickets/...
pub fn tickets_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{ticket_id}")
            .wrap(middleware::from_fn(check_ticket_middleware))
            .route("", web::get().to(get_ticket))
            .route("/send", web::post().to(send_ticket_message))
            .route("/close", web::post().to(close_ticket))
            .route(
                "/messages/{message_id}/purchase",
                web::post().to(purchase_ticket_message),
            )
            .default_service(web::route().to(error_handler)),
    );
}

// /messages/... - purchase, delete, ...

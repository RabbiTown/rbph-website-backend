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
use validator::Validate;

use crate::{
    AppState,
    api::{error_handler, puzzle::PuzzlePathInfo},
    db::{
        self,
        ticket::{SendMessageData, TicketMessage, TicketSummary, TicketThread, TicketUserInfo},
    },
    error::RbError,
    extractor::auth::AuthUser,
    middleware::privilege::PrivilegeMiddleware,
    model::game::{RbContentType, RbTicketSenderType, RbTicketState},
    model::user::RbUserRole,
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

#[derive(Deserialize)]
struct StaffDmPathInfo {
    game_id: i32,
    team_id: i32,
}

#[derive(Deserialize)]
struct StaffPuzzleTeamPathInfo {
    game_id: i32,
    puzzle_id: i32,
    team_id: i32,
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
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let result = db::ticket::get_dm_ticket_thread(
        &app.db,
        team_id,
        user.req_role()?.is_moderator(),
        user.req_role()?.is_admin(),
    )
    .await?;
    let message_ids = result
        .messages()
        .iter()
        .filter_map(db::ticket::TicketThreadItem::message_id)
        .collect::<Vec<_>>();
    if db::notification::mark_ticket_messages_read(&app.db, team_id, &message_ids).await? {
        app.sync_hub
            .notify_notification_updated(&app.db, team_id, None, "read")
            .await?;
    }

    Ok(HttpResponse::Ok().json(result))
}

// -- send --

fn def_content_type() -> RbContentType {
    RbContentType::UnsafeMarkdown
}

fn def_sender_type() -> RbTicketSenderType {
    RbTicketSenderType::Team
}

#[derive(Deserialize, Validate)]
struct TicketSendRequest {
    #[validate(length(min = 1, max = 1000))]
    content: String,
    #[serde(default = "def_content_type")]
    content_type: RbContentType,
    #[serde(default = "def_sender_type")]
    sender_type: RbTicketSenderType,
    #[serde(default)]
    cost_id: Option<i32>,
    #[serde(default)]
    cost_amount: i64,
    #[serde(default)]
    force_assignee: bool,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketSendResult {
    AssignedToOther = -7,
    Invalid = -6,
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

#[derive(Serialize)]
struct TicketAssigneeConflictResponse {
    code: TicketSendResult,
    assignee: db::ticket::TicketAggreInfoUser,
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

    req.validate()
        .map_err(|e| RbError::bad_req(TicketSendResult::Invalid.into()).msg(e.to_string()))?;

    if !user.req_role()?.is_admin() {
        if req.content_type.is_trusted() {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
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

    let force_assignee = req.force_assignee;
    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type: req.sender_type,
        sender_id: user.uid,
        cost_id: req.cost_id,
        cost_amount: req.cost_amount,
    };

    let msg = match db::ticket::send_ticket_message(
        &app.db,
        info.ticket_id,
        &data,
        max_pending,
        force_assignee,
    )
    .await?
    {
        db::ticket::SendTicketMessageResult::Ok(message) => message,
        db::ticket::SendTicketMessageResult::Pending => {
            return RbError::conflict(TicketSendResult::PendingExists.into()).http_err();
        }
        db::ticket::SendTicketMessageResult::Assigned(assignee) => {
            return Ok(
                HttpResponse::Conflict().json(TicketAssigneeConflictResponse {
                    code: TicketSendResult::AssignedToOther,
                    assignee,
                }),
            );
        }
    };
    app.sync_hub
        .notify_ticket_updated(&app.db, info.ticket_id, "message", Some(msg.id), user.uid)
        .await?;
    if matches!(data.sender_type, RbTicketSenderType::Host) {
        app.sync_hub
            .notify_notification_created_by_source(
                &app.db,
                db::notification::NotificationKind::TicketReply,
                msg.id,
            )
            .await?;
    }
    let mut ticket = db::ticket::get_ticket_summary(&app.db, info.ticket_id, true).await?;
    if !info.mod_access
        && let Some(ticket) = ticket.as_mut()
    {
        ticket.hide_assignee();
    }
    let send_block = db::ticket::calc_send_block(
        &app.db,
        info.ticket_id,
        info.state,
        info.member_access,
        max_pending,
    )
    .await?;
    let currency = if info.mod_access {
        ticket
            .as_ref()
            .map(TicketSummary::currency_ids)
            .unwrap_or_default()
    } else {
        vec![]
    };
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
        RbTicketSenderType::Team,
    )
    .await?;

    let info = db::ticket::get_ticket_user_info(&app.db, ticket_id, user.uid)
        .await?
        .ok_or(RbError::internal("Invalid ticket id"))?;

    do_send_ticket_message(req.into_inner(), &info, &user, &app, Some(3)).await
}

async fn send_staff_dm_ticket_message(
    path: web::Path<StaffDmPathInfo>,
    req: web::Json<TicketSendRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_game_id =
        sqlx::query_scalar!("SELECT game_id FROM rb_team WHERE id = $1", path.team_id)
            .fetch_optional(&app.db)
            .await
            .map_err(crate::error::RbInternalError::from)?;
    if team_game_id != Some(path.game_id) {
        return RbError::not_found().http_err();
    }

    let ticket_id = db::ticket::get_or_create_dm_ticket_id(
        &app.db,
        path.team_id,
        user.uid,
        RbTicketSenderType::Host,
    )
    .await?;
    let info = db::ticket::get_ticket_user_info(&app.db, ticket_id, user.uid)
        .await?
        .ok_or(RbError::internal("Invalid ticket id"))?;
    let mut req = req.into_inner();
    req.sender_type = RbTicketSenderType::Host;
    req.cost_id = None;
    req.cost_amount = 0;
    do_send_ticket_message(req, &info, &user, &app, None).await
}

async fn send_staff_dm_ticket_message_by_id(
    req: web::Json<TicketSendRequest>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut req = req.into_inner();
    req.sender_type = RbTicketSenderType::Host;
    req.cost_id = None;
    req.cost_amount = 0;
    do_send_ticket_message(req, &info, &user, &app, None).await
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

    let req = req.map(web::Json::into_inner);
    let force_assignee = req.as_ref().is_some_and(|req| req.force_assignee);
    let message = req.filter(|req| !req.content.is_empty());

    if let Some(message) = message.as_ref() {
        message
            .validate()
            .map_err(|e| RbError::bad_req(TicketSendResult::Invalid.into()).msg(e.to_string()))?;

        if !matches!(message.sender_type, RbTicketSenderType::Host) {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
        }
        if !user.req_role()?.is_admin() && message.content_type.is_trusted() {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
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

    let close_result = db::ticket::close_ticket(
        &app.db,
        path.ticket_id,
        user.uid,
        RbTicketSenderType::Host,
        message.as_ref(),
        force_assignee,
    )
    .await?;
    let close_message_id = match close_result {
        db::ticket::CloseTicketResult::Ok(message_id) => message_id,
        db::ticket::CloseTicketResult::Closed => {
            return RbError::conflict(TicketCloseResult::Closed.into()).http_err();
        }
        db::ticket::CloseTicketResult::Assigned(assignee) => {
            return Ok(
                HttpResponse::Conflict().json(TicketAssigneeConflictResponse {
                    code: TicketSendResult::AssignedToOther,
                    assignee,
                }),
            );
        }
    };
    app.sync_hub
        .notify_ticket_updated(
            &app.db,
            path.ticket_id,
            if close_message_id.is_some() {
                "message"
            } else {
                "closed"
            },
            close_message_id,
            user.uid,
        )
        .await?;
    if let Some(message_id) = close_message_id {
        app.sync_hub
            .notify_notification_created_by_source(
                &app.db,
                db::notification::NotificationKind::TicketReply,
                message_id,
            )
            .await?;
    }

    let ticket = db::ticket::get_ticket_summary(&app.db, path.ticket_id, true).await?;
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

    let purchase_result = db::ticket::purchase_ticket_message(
        &app,
        !info.mod_access,
        user.uid,
        path.ticket_id,
        path.message_id,
    )
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

    open_ticket_for_team(
        req.into_inner(),
        team_id,
        path.puzzle_id,
        RbTicketSenderType::Team,
        user,
        app,
    )
    .await
}

async fn open_ticket_for_team(
    req: TicketSendRequest,
    team_id: i32,
    puzzle_id: i32,
    sender_type: RbTicketSenderType,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    req.validate()
        .map_err(|e| RbError::bad_req(TicketSendResult::Invalid.into()).msg(e.to_string()))?;

    if (matches!(sender_type, RbTicketSenderType::Team)
        && !matches!(req.sender_type, RbTicketSenderType::Team))
        || req.cost_id.is_some()
    {
        RbError::unprocessable(TicketOpenResult::Invalid.into()).err()?
    }

    if !user.req_role()?.is_admin() && req.content_type.is_trusted() {
        RbError::unprocessable(TicketOpenResult::BadContentType.into()).err()?
    }

    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type,
        sender_id: user.uid,
        cost_id: None,
        cost_amount: 0,
    };

    let result = db::ticket::open_puzzle_ticket(&app.db, team_id, puzzle_id, &data).await?;

    match result {
        db::ticket::OpenPuzzleTicketResult::PendingExists => {
            RbError::conflict(TicketOpenResult::PendingExists.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::Cooldown => {
            RbError::conflict(TicketOpenResult::Cooldown.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::Disabled => {
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

            app.sync_hub
                .notify_ticket_updated(&app.db, ticket.id(), "created", Some(msg), user.uid)
                .await?;
            if matches!(sender_type, RbTicketSenderType::Host) {
                app.sync_hub
                    .notify_notification_created_by_source(
                        &app.db,
                        db::notification::NotificationKind::TicketReply,
                        msg,
                    )
                    .await?;
            }

            Ok(HttpResponse::Ok().json(TicketOpenResponse {
                code: TicketOpenResult::Ok,
                ticket_id: ticket.id(),
                message_id: msg,
                thread,
            }))
        }
    }
}

async fn staff_puzzle_team_exists(
    app: &AppState,
    path: &StaffPuzzleTeamPathInfo,
    user: &AuthUser,
) -> Result<bool> {
    if !user.req_role()?.is_admin() && user.req_team_id()? != Some(path.team_id) {
        return Ok(false);
    }
    Ok(sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1
            FROM rb_team t
            JOIN rb_puzzle p ON p.id = $3
            JOIN rb_round r ON r.id = p.round_id
            WHERE t.id = $2 AND t.game_id = $1 AND r.game_id = $1
        )",
        path.game_id,
        path.team_id,
        path.puzzle_id,
    )
    .fetch_one(&app.db)
    .await
    .map_err(crate::error::RbInternalError::from)?
    .unwrap_or(false))
}

async fn get_staff_team_puzzle_tickets(
    path: web::Path<StaffPuzzleTeamPathInfo>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !staff_puzzle_team_exists(&app, &path, &user).await? {
        return RbError::not_found().http_err();
    }
    let result = db::ticket::get_team_puzzle_tickets(&app.db, path.team_id, path.puzzle_id).await?;
    Ok(HttpResponse::Ok().json(result))
}

async fn open_staff_team_puzzle_ticket(
    path: web::Path<StaffPuzzleTeamPathInfo>,
    req: web::Json<TicketSendRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !staff_puzzle_team_exists(&app, &path, &user).await? {
        return RbError::not_found().http_err();
    }
    open_ticket_for_team(
        req.into_inner(),
        path.team_id,
        path.puzzle_id,
        RbTicketSenderType::Host,
        user,
        app,
    )
    .await
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

#[derive(Deserialize)]
struct StaffTicketListQuery {
    kind: Option<String>,
    state: Option<String>,
    waiting_for: Option<String>,
    assignee: Option<String>,
    puzzle_id: Option<i32>,
    team_id: Option<i32>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct StaffTicketListResponse {
    tickets: Vec<TicketSummary>,
}

#[derive(Deserialize)]
struct StaffTeamListQuery {
    search: Option<String>,
}

#[derive(Serialize)]
struct StaffTeamListItem {
    id: i32,
    name: String,
    state: i16,
}

#[derive(Serialize)]
struct StaffTeamListResponse {
    teams: Vec<StaffTeamListItem>,
}

async fn list_staff_teams(
    path: web::Path<crate::api::game::GamePathInfo>,
    query: web::Query<StaffTeamListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let search = query.search.as_deref().unwrap_or("").trim();
    let teams = sqlx::query_as!(
        StaffTeamListItem,
        "SELECT id, name, state FROM rb_team
        WHERE game_id = $1 AND ($2 = '' OR name ILIKE '%' || $2 || '%')
        ORDER BY name, id
        LIMIT 50",
        path.game_id,
        search,
    )
    .fetch_all(&app.db)
    .await
    .map_err(crate::error::RbInternalError::from)?;
    Ok(HttpResponse::Ok().json(StaffTeamListResponse { teams }))
}

async fn list_staff_tickets(
    path: web::Path<crate::api::game::GamePathInfo>,
    query: web::Query<StaffTicketListQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let kind = match query.kind.as_deref().unwrap_or("all") {
        "all" => 0,
        "puzzle" => 1,
        "dm" => 2,
        _ => return RbError::bad_req(-1).http_err(),
    };
    let state = match query.state.as_deref() {
        None | Some("all") => None,
        Some("open") => Some(i16::from(RbTicketState::Open)),
        Some("closed") => Some(i16::from(RbTicketState::Closed)),
        _ => return RbError::bad_req(-1).http_err(),
    };
    let waiting_for = match query.waiting_for.as_deref().unwrap_or("all") {
        "all" => 0,
        "staff" => 1,
        "team" => 2,
        _ => return RbError::bad_req(-1).http_err(),
    };
    let assignee = match query.assignee.as_deref().unwrap_or("all") {
        "all" => 0,
        "me" => 1,
        "none" => 2,
        _ => return RbError::bad_req(-1).http_err(),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0).max(0);
    let tickets = db::ticket::list_staff_tickets(
        &app.db,
        path.game_id,
        kind,
        state,
        waiting_for,
        assignee,
        user.uid,
        query.puzzle_id,
        query.team_id,
        limit,
        offset,
    )
    .await?;
    Ok(HttpResponse::Ok().json(StaffTicketListResponse { tickets }))
}

#[derive(Deserialize)]
struct TicketAssignRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
struct TicketAssignResponse {
    assignee: Option<db::ticket::TicketAggreInfoUser>,
}

async fn assign_ticket_self(
    path: web::Path<TicketPathInfo>,
    req: web::Json<TicketAssignRequest>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !info.mod_access {
        return RbError::forbid().http_err();
    }
    match db::ticket::assign_ticket_self(&app.db, path.ticket_id, user.uid, req.force).await? {
        db::ticket::AssignTicketResult::Ok(assignee) => {
            app.sync_hub
                .notify_ticket_updated(&app.db, path.ticket_id, "assigned", None, user.uid)
                .await?;
            Ok(HttpResponse::Ok().json(TicketAssignResponse {
                assignee: Some(assignee),
            }))
        }
        db::ticket::AssignTicketResult::Assigned(assignee) => Ok(HttpResponse::Conflict().json(
            TicketAssigneeConflictResponse {
                code: TicketSendResult::AssignedToOther,
                assignee,
            },
        )),
        db::ticket::AssignTicketResult::NotFound => RbError::not_found().http_err(),
    }
}

async fn unassign_ticket(
    path: web::Path<TicketPathInfo>,
    info: TicketUserInfo,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !info.mod_access {
        return RbError::forbid().http_err();
    }
    if !db::ticket::unassign_ticket(&app.db, path.ticket_id, user.uid).await? {
        return RbError::conflict(TicketSendResult::AssignedToOther.into()).http_err();
    }
    app.sync_hub
        .notify_ticket_updated(&app.db, path.ticket_id, "unassigned", None, user.uid)
        .await?;
    Ok(HttpResponse::Ok().json(TicketAssignResponse { assignee: None }))
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
        .filter(|info| info.member_access || info.mod_access)
        .ok_or_else(RbError::not_found)?;
    let is_puzzle_ticket = db::ticket::get_ticket_summary(&app.db, ticket_id, false)
        .await?
        .is_some_and(|ticket| ticket.is_puzzle_ticket());
    if !is_puzzle_ticket {
        return Err(RbError::not_found().into());
    }

    req.extensions_mut().insert(info);

    next.call(req).await
}

/// Check if a moderator is accessing a station-mail ticket in the requested game.
async fn check_staff_dm_ticket_middleware(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let ticket_id: i32 = req
        .match_info()
        .get("ticket_id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(RbError::not_found)?;
    let game_id: i32 = req
        .match_info()
        .get("game_id")
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
        .filter(|info| info.mod_access)
        .ok_or_else(RbError::not_found)?;
    let is_game_dm_ticket = db::ticket::get_ticket_summary(&app.db, ticket_id, false)
        .await?
        .is_some_and(|ticket| !ticket.is_puzzle_ticket() && ticket.game_id() == Some(game_id));
    if !is_game_dm_ticket {
        return Err(RbError::not_found().into());
    }

    req.extensions_mut().insert(info);

    next.call(req).await
}

pub fn games_config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("self")
            .route("", web::get().to(get_dm_ticket))
            .route("/send", web::post().to(send_dm_ticket_message))
            .default_service(web::route().to(error_handler)),
    )
    .service(
        web::scope("staff")
            .wrap(PrivilegeMiddleware::new(RbUserRole::Moderator))
            .route("", web::get().to(list_staff_tickets))
            .route("/teams", web::get().to(list_staff_teams))
            .route(
                "/puzzle/{puzzle_id}/teams/{team_id}",
                web::get().to(get_staff_team_puzzle_tickets),
            )
            .route(
                "/puzzle/{puzzle_id}/teams/{team_id}",
                web::post().to(open_staff_team_puzzle_ticket),
            )
            .route(
                "/dm/{team_id}/send",
                web::post().to(send_staff_dm_ticket_message),
            )
            .service(
                web::scope("/dm/tickets/{ticket_id}")
                    .wrap(middleware::from_fn(check_staff_dm_ticket_middleware))
                    .route("", web::get().to(get_ticket))
                    .route("/send", web::post().to(send_staff_dm_ticket_message_by_id))
                    .route("/assignee/self", web::post().to(assign_ticket_self))
                    .route("/assignee", web::delete().to(unassign_ticket))
                    .default_service(web::route().to(error_handler)),
            )
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
            .route("/assignee/self", web::post().to(assign_ticket_self))
            .route("/assignee", web::delete().to(unassign_ticket))
            .route(
                "/messages/{message_id}/purchase",
                web::post().to(purchase_ticket_message),
            )
            .default_service(web::route().to(error_handler)),
    );
}

// /messages/... - purchase, delete, ...

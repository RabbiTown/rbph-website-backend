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

#[derive(Deserialize, Default)]
struct TicketThreadQuery {
    before: Option<String>,
    after: Option<String>,
    stop: Option<String>,
}

fn ticket_page_request(query: &TicketThreadQuery) -> Result<db::ticket::TicketPageRequest> {
    if (query.before.is_some() && query.after.is_some())
        || (query.stop.is_some() && query.after.is_none())
    {
        return Err(RbError::bad_req(-1).into());
    }
    let decode = |value: &Option<String>| -> Result<Option<db::ticket::TicketCursor>> {
        match value {
            Some(value) => db::ticket::TicketCursor::decode(value)
                .map(Some)
                .ok_or_else(|| RbError::bad_req(-1).into()),
            None => Ok(None),
        }
    };
    Ok(db::ticket::TicketPageRequest {
        before: decode(&query.before)?,
        after: decode(&query.after)?,
        stop: decode(&query.stop)?,
        ..Default::default()
    })
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

#[derive(Deserialize)]
struct StaffPuzzleHintPathInfo {
    game_id: i32,
    puzzle_id: i32,
    team_id: i32,
    hint_id: i32,
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
    query: web::Query<TicketThreadQuery>,
    info: TicketUserInfo,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let page = ticket_page_request(&query)?;
    let result = db::ticket::get_ticket_thread(&app.db, path.ticket_id, &info, &page).await?;
    if result.is_none() {
        RbError::not_found().err()?
    }

    Ok(HttpResponse::Ok().json(result))
}

async fn get_dm_ticket(
    query: web::Query<TicketThreadQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let page = ticket_page_request(&query)?;
    let result = db::ticket::get_dm_ticket_thread(
        &app.db,
        team_id,
        user.req_role()?.is_moderator(),
        user.req_role()?.is_admin(),
        &page,
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
    unlock_after_seconds: i32,
    #[serde(default)]
    force_assignee: bool,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketSendResult {
    FeatureExistingOnly = -10,
    TeamFeatureBanned = -9,
    FeatureClosed = -8,
    AssignedToOther = -7,
    Invalid = -6,
    BadCost = -5,
    ContentTooLong = -4,
    BadContentType = -3,
    PendingExists = -2,
    Closed = -1,
    Ok = 0,
}

fn feature_access_error(
    access: db::ticket::PlayerFeatureAccess,
    has_existing: bool,
) -> Option<TicketSendResult> {
    match access.send_block(has_existing) {
        db::ticket::TicketSendBlock::FeatureClosed => Some(TicketSendResult::FeatureClosed),
        db::ticket::TicketSendBlock::FeatureExistingOnly => {
            Some(TicketSendResult::FeatureExistingOnly)
        }
        db::ticket::TicketSendBlock::TeamFeatureBanned => Some(TicketSendResult::TeamFeatureBanned),
        _ => None,
    }
}

fn normalize_unlock_values(req: &mut TicketSendRequest) -> bool {
    if req.cost_amount < 0 || req.unlock_after_seconds < 0 {
        return false;
    }

    if req.cost_amount == 0 {
        req.cost_id = None;
    } else if req.cost_id.is_none() {
        return false;
    }

    if !matches!(req.sender_type, RbTicketSenderType::Host)
        && (req.cost_id.is_some() || req.unlock_after_seconds > 0)
    {
        return false;
    }
    true
}

async fn normalize_unlock_requirements(
    req: &mut TicketSendRequest,
    team_id: i32,
    app: &AppState,
) -> Result<()> {
    if !normalize_unlock_values(req) {
        RbError::unprocessable(TicketSendResult::BadCost.into()).err()?
    }

    if let Some(currency_id) = req.cost_id {
        let valid = sqlx::query_scalar!(
            "SELECT EXISTS (
                SELECT 1
                FROM rb_team_currency tc
                JOIN rb_currency c ON c.id = tc.currency_id
                JOIN rb_game_feature gf ON gf.game_id = c.game_id AND gf.feature_type = 4
                JOIN rb_team t ON t.id = tc.team_id AND t.game_id = c.game_id
                WHERE tc.team_id = $1 AND c.id = $2 AND NOT tc.hidden AND gf.state = 1
            ) AS \"valid!\"",
            team_id,
            currency_id,
        )
        .fetch_one(&app.db)
        .await
        .map_err(crate::error::RbInternalError::from)?;
        if !valid {
            RbError::unprocessable(TicketSendResult::BadCost.into()).err()?
        }
    }

    Ok(())
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
    mut req: TicketSendRequest,
    info: &TicketUserInfo,
    user: &AuthUser,
    app: &AppState,
    max_pending: Option<i64>,
) -> Result<HttpResponse> {
    crate::module::release::process_due_releases(app).await?;
    let accessible = match req.sender_type {
        RbTicketSenderType::Team => info.member_access,
        RbTicketSenderType::Host => info.mod_access,
        _ => false,
    };
    if !accessible {
        RbError::forbid().err()?
    }

    if matches!(req.sender_type, RbTicketSenderType::Team)
        && let Some(result) = feature_access_error(
            db::ticket::player_ticket_feature_access(&app.db, info.ticket_id).await?,
            true,
        )
    {
        return RbError::conflict(result.into()).http_err();
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

    let team_id = sqlx::query_scalar!(
        "SELECT team_id FROM rb_ticket WHERE id = $1",
        info.ticket_id
    )
    .fetch_one(&app.db)
    .await
    .map_err(crate::error::RbInternalError::from)?;
    normalize_unlock_requirements(&mut req, team_id, app).await?;

    let force_assignee = req.force_assignee;
    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type: req.sender_type,
        sender_id: user.uid,
        cost_id: req.cost_id,
        cost_amount: req.cost_amount,
        unlock_after_seconds: req.unlock_after_seconds,
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
    let feature_block = db::ticket::player_ticket_feature_access(&app.db, info.ticket_id)
        .await?
        .send_block(true);
    let send_block = if info.member_access && feature_block != db::ticket::TicketSendBlock::Ok {
        feature_block
    } else {
        db::ticket::calc_send_block(
            &app.db,
            info.ticket_id,
            info.state,
            info.member_access,
            max_pending,
        )
        .await?
    };
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
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let has_existing = db::ticket::get_dm_ticket_id(&app.db, team_id)
        .await?
        .is_some();
    if let Some(result) = feature_access_error(
        db::ticket::player_dm_feature_access(&app.db, team_id).await?,
        has_existing,
    ) {
        return RbError::conflict(result.into()).http_err();
    }
    let ticket_id = db::ticket::get_or_create_dm_ticket_id(
        &app.db,
        team_id,
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

    let mut req = req.map(web::Json::into_inner);
    let force_assignee = req.as_ref().is_some_and(|req| req.force_assignee);

    if let Some(message) = req.as_mut().filter(|req| !req.content.is_empty()) {
        message
            .validate()
            .map_err(|e| RbError::bad_req(TicketSendResult::Invalid.into()).msg(e.to_string()))?;

        if !matches!(message.sender_type, RbTicketSenderType::Host) {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
        }
        if !user.req_role()?.is_admin() && message.content_type.is_trusted() {
            RbError::unprocessable(TicketSendResult::BadContentType.into()).err()?
        }
        let team_id = sqlx::query_scalar!(
            "SELECT team_id FROM rb_ticket WHERE id = $1",
            path.ticket_id
        )
        .fetch_one(&app.db)
        .await
        .map_err(crate::error::RbInternalError::from)?;
        normalize_unlock_requirements(message, team_id, &app).await?;
    }

    let message = req
        .filter(|req| !req.content.is_empty())
        .map(|message| SendMessageData {
            content: message.content,
            content_type: message.content_type,
            sender_type: RbTicketSenderType::Host,
            sender_id: user.uid,
            cost_id: message.cost_id,
            cost_amount: message.cost_amount,
            unlock_after_seconds: message.unlock_after_seconds,
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
    let thread = db::ticket::get_ticket_thread(
        &app.db,
        path.ticket_id,
        &refreshed_info,
        &db::ticket::TicketPageRequest::default(),
    )
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
    TeamFeatureBanned = -8,
    FeatureExistingOnly = -7,
    FeatureClosed = -6,
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
    crate::module::release::process_due_releases(app.get_ref()).await?;
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
    mut req: TicketSendRequest,
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
    {
        RbError::unprocessable(TicketOpenResult::Invalid.into()).err()?
    }

    if !user.req_role()?.is_admin() && req.content_type.is_trusted() {
        RbError::unprocessable(TicketOpenResult::BadContentType.into()).err()?
    }
    req.sender_type = sender_type;
    normalize_unlock_requirements(&mut req, team_id, &app).await?;

    let data = SendMessageData {
        content: req.content,
        content_type: req.content_type,
        sender_type,
        sender_id: user.uid,
        cost_id: req.cost_id,
        cost_amount: req.cost_amount,
        unlock_after_seconds: req.unlock_after_seconds,
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
        db::ticket::OpenPuzzleTicketResult::FeatureClosed => {
            RbError::conflict(TicketOpenResult::FeatureClosed.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::FeatureExistingOnly => {
            RbError::conflict(TicketOpenResult::FeatureExistingOnly.into()).http_err()
        }
        db::ticket::OpenPuzzleTicketResult::TeamFeatureBanned => {
            RbError::conflict(TicketOpenResult::TeamFeatureBanned.into()).http_err()
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
    let result =
        db::ticket::get_team_puzzle_tickets(&app.db, path.team_id, path.puzzle_id, true).await?;
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
    crate::module::release::process_due_releases(app.get_ref()).await?;
    let team_id = user.req_team_id()?.ok_or(RbError::forbid())?;
    let result =
        db::ticket::get_team_puzzle_tickets(&app.db, team_id, path.puzzle_id, false).await?;

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
    has_more: bool,
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

#[derive(Deserialize)]
struct StaffTeamAccessPath {
    game_id: i32,
    team_id: i32,
}

#[derive(Deserialize)]
struct StaffTeamFeatureUpdate {
    feature: db::feature::GameFeature,
    enabled: bool,
}

#[derive(Deserialize, Validate)]
struct StaffTeamAccessUpdateRequest {
    is_banned: Option<bool>,
    is_locked: Option<bool>,
    features: Option<Vec<StaffTeamFeatureUpdate>>,
    #[validate(length(max = 500))]
    reason: Option<String>,
}

#[derive(Serialize)]
struct StaffTeamAccessData {
    team_id: i32,
    is_banned: bool,
    is_locked: bool,
    features: Vec<db::team::RbTeamFeatureData>,
}

#[derive(Serialize)]
struct StaffTeamAccessCapabilities {
    team_ban: bool,
    team_lock: bool,
    features: Vec<db::feature::GameFeature>,
}

#[derive(Serialize)]
struct StaffTeamAccessResponse {
    access: StaffTeamAccessData,
    editable: StaffTeamAccessCapabilities,
}

#[derive(Deserialize, Validate)]
struct StaffCurrencyAdjustRequest {
    delta: i64,
    #[validate(length(max = 500))]
    reason: Option<String>,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum StaffCurrencyAdjustCode {
    Invalid = -3,
    AboveMax = -1,
    Ok = 0,
}

#[derive(Serialize)]
struct StaffCurrencyListResponse {
    currencies: Vec<db::team::RbCurrencyShowData>,
}

#[derive(Deserialize)]
struct StaffManagementActivityQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct StaffCurrencyResponse {
    code: StaffCurrencyAdjustCode,
    currency: db::team::RbCurrencyShowData,
}

#[derive(Debug, Eq, PartialEq)]
enum StaffTeamAccessValidation {
    Valid,
    Invalid,
    Forbidden,
}

fn validate_staff_team_access_update(
    role: RbUserRole,
    req: &StaffTeamAccessUpdateRequest,
) -> StaffTeamAccessValidation {
    let mut features = std::collections::HashSet::new();
    let valid_features = req.features.as_ref().is_none_or(|updates| {
        updates.iter().all(|update| {
            let valid = matches!(
                update.feature,
                db::feature::GameFeature::DirectMessage
                    | db::feature::GameFeature::PuzzleTicket
                    | db::feature::GameFeature::Leaderboard
            );
            valid && features.insert(update.feature.value())
        })
    });
    if !valid_features {
        return StaffTeamAccessValidation::Invalid;
    }

    if !role.is_admin()
        && (req.is_banned.is_some()
            || req.is_locked.is_some()
            || req.features.as_ref().is_some_and(|updates| {
                updates
                    .iter()
                    .any(|update| update.feature == db::feature::GameFeature::Leaderboard)
            }))
    {
        return StaffTeamAccessValidation::Forbidden;
    }

    StaffTeamAccessValidation::Valid
}

fn staff_team_access_response(
    team: db::team::AdminTeamDetail,
    role: RbUserRole,
) -> StaffTeamAccessResponse {
    let is_admin = role.is_admin();
    StaffTeamAccessResponse {
        access: StaffTeamAccessData {
            team_id: team.id,
            is_banned: team.is_banned,
            is_locked: team.is_locked,
            features: team.features,
        },
        editable: StaffTeamAccessCapabilities {
            team_ban: is_admin,
            team_lock: is_admin,
            features: if is_admin {
                vec![
                    db::feature::GameFeature::DirectMessage,
                    db::feature::GameFeature::PuzzleTicket,
                    db::feature::GameFeature::Leaderboard,
                ]
            } else {
                vec![
                    db::feature::GameFeature::DirectMessage,
                    db::feature::GameFeature::PuzzleTicket,
                ]
            },
        },
    }
}

async fn get_staff_team_access(
    path: web::Path<StaffTeamAccessPath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let role = user.req_role()?;
    let team = db::team::admin_get(&app.db, path.game_id, path.team_id).await?;
    let Some(team) = team else {
        return RbError::not_found().http_err();
    };
    Ok(HttpResponse::Ok().json(staff_team_access_response(team, role)))
}

async fn update_staff_team_access(
    path: web::Path<StaffTeamAccessPath>,
    req: web::Json<StaffTeamAccessUpdateRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if let Err(error) = req.validate() {
        return RbError::bad_req(-1).msg(error.to_string()).http_err();
    }
    let role = user.req_role()?;
    match validate_staff_team_access_update(role, &req) {
        StaffTeamAccessValidation::Valid => {}
        StaffTeamAccessValidation::Invalid => return RbError::bad_req(-1).http_err(),
        StaffTeamAccessValidation::Forbidden => return RbError::forbid().http_err(),
    }

    let update = db::team::AdminTeamUpdateData {
        name: None,
        pass: None,
        bio: None,
        is_banned: req.is_banned,
        is_locked: req.is_locked,
        features: req.features.as_ref().map(|features| {
            features
                .iter()
                .map(|feature| db::team::AdminTeamFeatureDataInput {
                    feature: feature.feature,
                    enabled: feature.enabled,
                })
                .collect()
        }),
        reason: req.reason.clone(),
    };
    let team =
        db::team::admin_update(&app.db, path.game_id, path.team_id, user.uid, &update).await?;
    let Some(team) = team else {
        return RbError::not_found().http_err();
    };
    db::cache::invalidate_team_info(&app, path.team_id).await?;
    db::board::LEADER_BOARD_CACHE
        .invalidate_game(&app.db, path.game_id)
        .await?;
    Ok(HttpResponse::Ok().json(staff_team_access_response(team, role)))
}

async fn get_staff_team_currencies(
    path: web::Path<StaffTeamAccessPath>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let team_exists = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM rb_team WHERE id = $1 AND game_id = $2) AS \"exists!\";",
        path.team_id,
        path.game_id
    )
    .fetch_one(&app.db)
    .await
    .map_err(crate::error::RbInternalError::from)?;
    if !team_exists {
        return RbError::not_found().http_err();
    }
    let currencies = db::team::get_currency_info(&app.db, path.team_id).await?;
    Ok(HttpResponse::Ok().json(StaffCurrencyListResponse { currencies }))
}

async fn adjust_staff_team_currency(
    path: web::Path<(i32, i32, i32)>,
    req: web::Json<StaffCurrencyAdjustRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let (game_id, team_id, currency_id) = path.into_inner();
    if req.delta == 0 {
        return RbError::bad_req(StaffCurrencyAdjustCode::Invalid.into()).http_err();
    }
    if let Err(error) = req.validate() {
        return RbError::bad_req(StaffCurrencyAdjustCode::Invalid.into())
            .msg(error.to_string())
            .http_err();
    }
    let result = db::team::staff_adjust_currency(
        &app.db,
        game_id,
        team_id,
        currency_id,
        req.delta,
        user.uid,
        req.reason.as_deref(),
    )
    .await?;
    let (code, currency) = match result {
        db::team::StaffCurrencyAdjustResult::NotFound => {
            return RbError::not_found().http_err();
        }
        db::team::StaffCurrencyAdjustResult::Overflow => {
            return RbError::bad_req(StaffCurrencyAdjustCode::Invalid.into()).http_err();
        }
        db::team::StaffCurrencyAdjustResult::AboveMax(currency) => {
            (StaffCurrencyAdjustCode::AboveMax, currency)
        }
        db::team::StaffCurrencyAdjustResult::Updated(currency) => {
            db::cache::invalidate_team_info(&app, team_id).await?;
            db::board::LEADER_BOARD_CACHE
                .invalidate_game(&app.db, game_id)
                .await?;
            return Ok(HttpResponse::Ok().json(StaffCurrencyResponse {
                code: StaffCurrencyAdjustCode::Ok,
                currency,
            }));
        }
    };
    Ok(HttpResponse::Conflict().json(StaffCurrencyResponse { code, currency }))
}

async fn get_staff_team_management_activity(
    path: web::Path<StaffTeamAccessPath>,
    query: web::Query<StaffManagementActivityQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let activity = db::event_log::list_team_management_activity(
        &app.db,
        path.game_id,
        path.team_id,
        query.before,
        query.limit.unwrap_or(30),
    )
    .await?;
    Ok(HttpResponse::Ok().json(activity))
}

async fn get_staff_puzzle_team_status(
    path: web::Path<StaffPuzzleTeamPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_staff_puzzle_team_status(
        &app.db,
        path.game_id,
        path.team_id,
        path.puzzle_id,
    )
    .await?;
    let Some(result) = result else {
        return RbError::not_found().http_err();
    };
    Ok(HttpResponse::Ok().json(result))
}

#[derive(Deserialize)]
struct StaffPuzzleSubmissionQuery {
    page: Option<i64>,
    only_ok: Option<bool>,
}

async fn get_staff_puzzle_submissions(
    path: web::Path<StaffPuzzleTeamPathInfo>,
    query: web::Query<StaffPuzzleSubmissionQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_staff_puzzle_submissions(
        &app.db,
        path.game_id,
        path.team_id,
        path.puzzle_id,
        query.page.unwrap_or(0).max(0),
        10,
        query.only_ok.unwrap_or(false),
    )
    .await?;
    let Some(result) = result else {
        return RbError::not_found().http_err();
    };
    Ok(HttpResponse::Ok().json(result))
}

async fn get_staff_puzzle_hint_content(
    path: web::Path<StaffPuzzleHintPathInfo>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::puzzle::get_staff_puzzle_hint_content(
        &app.db,
        path.game_id,
        path.team_id,
        path.puzzle_id,
        path.hint_id,
    )
    .await?;
    let Some(result) = result else {
        return RbError::not_found().http_err();
    };
    Ok(HttpResponse::Ok().json(result))
}

async fn list_staff_teams(
    path: web::Path<crate::api::game::GamePathInfo>,
    query: web::Query<StaffTeamListQuery>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let search = query.search.as_deref().unwrap_or("").trim();
    let teams = sqlx::query_as!(
        StaffTeamListItem,
        "SELECT id, name,
            (CASE
                WHEN is_banned THEN -1
                WHEN finish_at IS NOT NULL THEN 2
                WHEN is_locked THEN 1
                ELSE 0
            END)::SMALLINT AS \"state!\"
        FROM rb_team
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
    let mut tickets = db::ticket::list_staff_tickets(
        &app.db,
        path.game_id,
        kind,
        state,
        waiting_for,
        assignee,
        user.uid,
        query.puzzle_id,
        query.team_id,
        limit + 1,
        offset,
    )
    .await?;
    let has_more = tickets.len() > limit as usize;
    tickets.truncate(limit as usize);
    Ok(HttpResponse::Ok().json(StaffTicketListResponse { tickets, has_more }))
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
                "/teams/{team_id}/access",
                web::get().to(get_staff_team_access),
            )
            .route(
                "/teams/{team_id}/access",
                web::patch().to(update_staff_team_access),
            )
            .route(
                "/teams/{team_id}/currencies",
                web::get().to(get_staff_team_currencies),
            )
            .route(
                "/teams/{team_id}/currencies/{currency_id}/adjust",
                web::post().to(adjust_staff_team_currency),
            )
            .route(
                "/teams/{team_id}/management-activity",
                web::get().to(get_staff_team_management_activity),
            )
            .route(
                "/puzzle/{puzzle_id}/teams/{team_id}/status",
                web::get().to(get_staff_puzzle_team_status),
            )
            .route(
                "/puzzle/{puzzle_id}/teams/{team_id}/status/submissions",
                web::get().to(get_staff_puzzle_submissions),
            )
            .route(
                "/puzzle/{puzzle_id}/teams/{team_id}/status/hints/{hint_id}",
                web::get().to(get_staff_puzzle_hint_content),
            )
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

#[cfg(test)]
mod ticket_unlock_tests {
    use super::{TicketSendRequest, normalize_unlock_values};
    use crate::model::game::{RbContentType, RbTicketSenderType};

    fn request(
        sender_type: RbTicketSenderType,
        cost_id: Option<i32>,
        cost_amount: i64,
        unlock_after_seconds: i32,
    ) -> TicketSendRequest {
        TicketSendRequest {
            content: "message".to_string(),
            content_type: RbContentType::UnsafeMarkdown,
            sender_type,
            cost_id,
            cost_amount,
            unlock_after_seconds,
            force_assignee: false,
        }
    }

    #[test]
    fn accepts_combined_staff_unlock_requirements() {
        let mut req = request(RbTicketSenderType::Host, Some(2), 100, 60);
        assert!(normalize_unlock_values(&mut req));
        assert_eq!(req.cost_id, Some(2));
        assert_eq!(req.cost_amount, 100);
        assert_eq!(req.unlock_after_seconds, 60);
    }

    #[test]
    fn normalizes_zero_cost_to_no_currency_requirement() {
        let mut req = request(RbTicketSenderType::Host, Some(2), 0, 60);
        assert!(normalize_unlock_values(&mut req));
        assert_eq!(req.cost_id, None);
    }

    #[test]
    fn rejects_invalid_or_team_unlock_requirements() {
        assert!(!normalize_unlock_values(&mut request(
            RbTicketSenderType::Host,
            None,
            100,
            0,
        )));
        assert!(!normalize_unlock_values(&mut request(
            RbTicketSenderType::Host,
            None,
            0,
            -1,
        )));
        assert!(!normalize_unlock_values(&mut request(
            RbTicketSenderType::Team,
            Some(2),
            100,
            0,
        )));
        assert!(!normalize_unlock_values(&mut request(
            RbTicketSenderType::Team,
            None,
            0,
            60,
        )));
    }
}

#[cfg(test)]
mod staff_team_access_tests {
    use super::{
        StaffTeamAccessUpdateRequest, StaffTeamAccessValidation, StaffTeamFeatureUpdate,
        validate_staff_team_access_update,
    };
    use crate::{db::feature::GameFeature, model::user::RbUserRole};
    use validator::Validate;

    fn request(features: Vec<GameFeature>) -> StaffTeamAccessUpdateRequest {
        StaffTeamAccessUpdateRequest {
            is_banned: None,
            is_locked: None,
            features: Some(
                features
                    .into_iter()
                    .map(|feature| StaffTeamFeatureUpdate {
                        feature,
                        enabled: false,
                    })
                    .collect(),
            ),
            reason: None,
        }
    }

    #[test]
    fn moderators_can_only_update_message_features() {
        assert_eq!(
            validate_staff_team_access_update(
                RbUserRole::Moderator,
                &request(vec![GameFeature::DirectMessage, GameFeature::PuzzleTicket]),
            ),
            StaffTeamAccessValidation::Valid,
        );
        assert_eq!(
            validate_staff_team_access_update(
                RbUserRole::Moderator,
                &request(vec![GameFeature::Leaderboard]),
            ),
            StaffTeamAccessValidation::Forbidden,
        );

        let mut team_update = request(vec![]);
        team_update.is_banned = Some(true);
        assert_eq!(
            validate_staff_team_access_update(RbUserRole::Moderator, &team_update),
            StaffTeamAccessValidation::Forbidden,
        );
    }

    #[test]
    fn administrators_can_update_complete_access_section() {
        let mut update = request(vec![
            GameFeature::DirectMessage,
            GameFeature::PuzzleTicket,
            GameFeature::Leaderboard,
        ]);
        update.is_banned = Some(true);
        update.is_locked = Some(true);
        assert_eq!(
            validate_staff_team_access_update(RbUserRole::Admin, &update),
            StaffTeamAccessValidation::Valid,
        );
        assert_eq!(
            validate_staff_team_access_update(RbUserRole::Root, &update),
            StaffTeamAccessValidation::Valid,
        );
    }

    #[test]
    fn rejects_duplicate_and_non_team_features() {
        assert_eq!(
            validate_staff_team_access_update(
                RbUserRole::Admin,
                &request(vec![GameFeature::DirectMessage, GameFeature::DirectMessage]),
            ),
            StaffTeamAccessValidation::Invalid,
        );
        assert_eq!(
            validate_staff_team_access_update(
                RbUserRole::Admin,
                &request(vec![GameFeature::Currency]),
            ),
            StaffTeamAccessValidation::Invalid,
        );
    }

    #[test]
    fn validates_optional_reason_length() {
        let mut update = request(vec![]);
        update.reason = Some("a".repeat(500));
        assert!(update.validate().is_ok());
        update.reason = Some("a".repeat(501));
        assert!(update.validate().is_err());
    }
}

// /messages/... - purchase, delete, ...

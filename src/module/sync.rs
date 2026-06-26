use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use actix_web::{HttpRequest, HttpResponse, web::Payload};
use actix_ws::{CloseCode, Message, MessageStream, Session};
use dashmap::DashMap;
use num_enum::IntoPrimitive;
use serde::Serialize;
use serde_json::json;
use serde_repr::Serialize_repr;
use time::OffsetDateTime;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::{
    DbPool, db,
    db::{
        puzzle::{CurrencyPenaltyShowData, RbPuzzleTeamStateShowData},
        team::RbCurrencyShowData,
    },
    error::RbInternalError,
    model::game::RbJudgeAction,
    serde_helpers::serialize_option_offset_datetime,
};

#[derive(Default)]
pub struct SyncHub {
    users: DashMap<i32, Vec<WsSessionHandle>>,
}

impl SyncHub {
    pub fn create_ws(
        &self,
        req: HttpRequest,
        stream: Payload,
        user_id: i32,
    ) -> actix_web::Result<HttpResponse> {
        let (resp, session, stream) = actix_ws::handle(&req, stream)?;
        let (handle, rx) = WsSessionHandle::new();
        self.users.entry(user_id).or_default().push(handle);
        actix_web::rt::spawn(WsSession::new(session, stream, rx).run());
        Ok(resp)
    }

    fn push_user<T: Serialize>(&self, user_id: i32, msg_type: SyncMessageType, data: T) {
        if let Some(addrs) = self.users.get(&user_id) {
            let envelope = WsEnvelope { msg_type, data };
            if let Ok(json) = serde_json::to_string(&envelope) {
                let arc_json = Arc::new(json);
                for session in addrs.iter() {
                    let _ = session.push(arc_json.clone());
                }
            }
        }
    }

    fn push_users<T: Serialize>(&self, users: &[i32], msg_type: SyncMessageType, data: T) {
        let envelope = WsEnvelope { msg_type, data };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let arc_json = Arc::new(json);
            for user_id in users {
                if let Some(addrs) = self.users.get(user_id) {
                    for session in addrs.iter() {
                        let _ = session.push(arc_json.clone());
                    }
                }
            }
        }
    }

    async fn push_team<T: Serialize>(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        msg_type: SyncMessageType,
        data: T,
    ) -> Result<(), RbInternalError> {
        let members = db::team::get_member_id(db_pool, team_id).await?;
        self.push_users(&members, msg_type, data);
        Ok(())
    }

    pub async fn notify_puzzle_submitted(
        &self,
        db_pool: &DbPool,
        event: PuzzleSubmittedSync,
    ) -> Result<(), RbInternalError> {
        let row = sqlx::query!(
            "SELECT
                (SELECT nickname FROM rb_user WHERE id = $1) AS u_n,
                (SELECT title FROM rb_puzzle WHERE id = $2) AS p_t;",
            event.user_id,
            event.puzzle_id
        )
        .fetch_one(db_pool)
        .await?;

        let mut sync = json!({
            "user": {
                "id": event.user_id,
                "name": row.u_n,
            },
            "puzzle": {
                "id": event.puzzle_id,
                "title": row.p_t,
            },
            "answer": event.answer,
            "action": event.action,
        });

        if let Some(sid) = event.sid {
            sync["sid"] = json!(sid);
        }
        if event.cooldown_till.is_some()
            && let Ok(x) = serialize_option_offset_datetime::serialize(
                &event.cooldown_till,
                serde_json::value::Serializer,
            )
        {
            sync["cooldown_till"] = x;
        }
        if let Some(state) = event.state {
            sync["state"] = json!(state);
        }
        if !event.currency.is_empty() {
            sync["currency"] = json!(event.currency);
        }
        if !event.currency_penalty.is_empty() {
            sync["currency_penalty"] = json!(event.currency_penalty);
        }
        if event.solved {
            sync["solved"] = json!(true);
            sync["unlocks"] = json!(event.unlocks);
        }

        self.push_team(
            db_pool,
            event.team_id,
            SyncMessageType::PuzzleSubmitted,
            sync,
        )
        .await
    }

    pub async fn notify_puzzle_hint_unlocked(
        &self,
        db_pool: &DbPool,
        event: PuzzleHintUnlockedSync,
    ) -> Result<(), RbInternalError> {
        let row = sqlx::query!(
            "SELECT (SELECT nickname FROM rb_user WHERE id = $1) AS u_n,
                    h.title AS h_t, h.cost_id AS h_ci, h.cost_amount AS h_ca,
                    p.title AS p_t, p.id AS p_i
            FROM rb_hint h
            JOIN rb_puzzle p ON p.id = h.puzzle_id
            WHERE h.id = $2",
            event.user_id,
            event.hint_id
        )
        .fetch_one(db_pool)
        .await?;

        let mut sync = json!({
            "user": {
                "id": event.user_id,
                "name": row.u_n,
            },
            "puzzle": {
                "id": row.p_i,
                "title": row.p_t,
            },
            "hint": {
                "id": event.hint_id,
                "title": row.h_t,
                "cost_id": row.h_ci,
                "cost_amount": row.h_ca
            }
        });
        if let Some(sid) = event.sid {
            sync["sid"] = json!(sid);
        }

        self.push_team(
            db_pool,
            event.team_id,
            SyncMessageType::PuzzleHintUnlocked,
            sync,
        )
        .await
    }

    pub async fn notify_team_info_updated(
        &self,
        db_pool: &DbPool,
        team_id: i32,
    ) -> Result<(), RbInternalError> {
        self.push_team(db_pool, team_id, SyncMessageType::TeamInfoUpdated, ())
            .await
    }

    pub fn notify_team_disbanded(&self, users: &[i32]) {
        self.push_users(users, SyncMessageType::TeamDisbanded, ());
    }

    pub fn notify_team_self_kicked(&self, user_id: i32) {
        self.push_user(user_id, SyncMessageType::TeamSelfKicked, ());
    }

    pub fn notify_team_self_promoted(&self, user_id: i32) {
        self.push_user(user_id, SyncMessageType::TeamSelfPromoted, ());
    }

    pub async fn notify_ticket_updated(
        &self,
        db_pool: &DbPool,
        ticket_id: i32,
        event: &str,
        message_id: Option<i32>,
        actor_id: i32,
    ) -> Result<(), RbInternalError> {
        let ticket = sqlx::query!(
            "SELECT tk.team_id, tk.puzzle_id, t.game_id
            FROM rb_ticket tk
            JOIN rb_team t ON t.id = tk.team_id
            WHERE tk.id = $1",
            ticket_id,
        )
        .fetch_one(db_pool)
        .await?;
        let moderators = sqlx::query_scalar!(
            "SELECT id FROM rb_user WHERE urole >= $1",
            i16::from(crate::model::user::RbUserRole::Moderator),
        )
        .fetch_all(db_pool)
        .await?;
        let data = json!({
            "event": event,
            "ticket_id": ticket_id,
            "message_id": message_id,
            "actor_id": actor_id,
            "team_id": ticket.team_id,
            "puzzle_id": ticket.puzzle_id,
            "game_id": ticket.game_id,
        });

        self.push_team(
            db_pool,
            ticket.team_id,
            SyncMessageType::TicketUpdated,
            data.clone(),
        )
        .await?;
        self.push_users(&moderators, SyncMessageType::TicketUpdated, data);
        Ok(())
    }

    pub async fn notify_notification_created_by_source(
        &self,
        db_pool: &DbPool,
        kind: crate::db::notification::NotificationKind,
        source_id: i32,
    ) -> Result<(), RbInternalError> {
        let Some(info) =
            crate::db::notification::get_sync_info_by_source(db_pool, kind, source_id).await?
        else {
            return Ok(());
        };
        let data = json!({
            "event": "created",
            "notification_id": info.id,
            "team_id": info.team_id,
            "game_id": info.game_id,
        });
        self.push_team(
            db_pool,
            info.team_id,
            SyncMessageType::NotificationUpdated,
            data,
        )
        .await
    }

    pub async fn notify_notification_updated(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        notification_id: Option<i64>,
        event: &str,
    ) -> Result<(), RbInternalError> {
        let game_id = sqlx::query_scalar!("SELECT game_id FROM rb_team WHERE id = $1", team_id)
            .fetch_one(db_pool)
            .await?;
        self.push_team(
            db_pool,
            team_id,
            SyncMessageType::NotificationUpdated,
            json!({
                "event": event,
                "notification_id": notification_id,
                "team_id": team_id,
                "game_id": game_id,
            }),
        )
        .await
    }

    pub fn cleanup(&self) {
        self.users.retain(|_, sessions| {
            sessions.retain(|session| !session.is_closed());
            for session in sessions.iter() {
                let _ = session.check_alive();
            }
            !sessions.is_empty()
        });
    }

    pub async fn invalidate(&self, user_id: i32) {
        if let Some((_, sessions)) = self.users.remove(&user_id) {
            for session in sessions.iter() {
                let _ = session.close();
            }
        }
    }
}

pub struct PuzzleSubmittedSync {
    pub team_id: i32,
    pub user_id: i32,
    pub puzzle_id: i32,
    pub answer: String,
    pub action: RbJudgeAction,
    pub cooldown_till: Option<OffsetDateTime>,
    pub solved: bool,
    pub unlocks: Vec<PuzzleUnlockInfo>,
    pub state: Option<RbPuzzleTeamStateShowData>,
    pub currency: Vec<RbCurrencyShowData>,
    pub currency_penalty: Vec<CurrencyPenaltyShowData>,
    pub sid: Option<String>,
}

pub struct PuzzleHintUnlockedSync {
    pub team_id: i32,
    pub user_id: i32,
    pub hint_id: i32,
    pub sid: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct PuzzleUnlockInfo {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub round_id: i32,
    pub round_slug: Option<String>,
}

#[derive(Clone)]
pub struct WsSessionHandle {
    tx: UnboundedSender<WsCommand>,
}

impl WsSessionHandle {
    fn new() -> (Self, UnboundedReceiver<WsCommand>) {
        let (tx, rx) = unbounded_channel();
        (Self { tx }, rx)
    }

    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    fn push(&self, msg: Arc<String>) -> Result<(), WsCommand> {
        self.tx.send(WsCommand::Push(msg)).map_err(|e| e.0)
    }

    fn check_alive(&self) -> Result<(), WsCommand> {
        self.tx.send(WsCommand::CheckAlive).map_err(|e| e.0)
    }

    fn close(&self) -> Result<(), WsCommand> {
        self.tx.send(WsCommand::Close).map_err(|e| e.0)
    }
}

enum WsCommand {
    Push(Arc<String>),
    CheckAlive,
    Close,
}

pub struct WsSession {
    session: Session,
    stream: MessageStream,
    commands: UnboundedReceiver<WsCommand>,
    last_heartbeat: Instant,
}

impl WsSession {
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

    fn new(
        session: Session,
        stream: MessageStream,
        commands: UnboundedReceiver<WsCommand>,
    ) -> Self {
        WsSession {
            session,
            stream,
            commands,
            last_heartbeat: Instant::now(),
        }
    }

    fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    async fn run(mut self) {
        loop {
            tokio::select! {
                msg = self.stream.recv() => {
                    let Some(msg) = msg else {
                        break;
                    };

                    if self.handle_message(msg).await {
                        break;
                    }
                }
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        break;
                    };

                    if self.handle_command(command).await {
                        break;
                    }
                }
            }
        }
    }

    async fn handle_message(&mut self, msg: Result<Message, actix_ws::ProtocolError>) -> bool {
        match msg {
            Ok(Message::Ping(msg)) => {
                self.heartbeat();
                self.session.pong(&msg).await.is_err()
            }
            Ok(Message::Pong(_)) | Ok(Message::Text(_)) | Ok(Message::Binary(_)) => {
                self.heartbeat();
                false
            }
            Ok(Message::Close(reason)) => {
                let _ = self.session.clone().close(reason).await;
                true
            }
            Err(e) => {
                log::warn!("ws error: {:?}", e);
                true
            }
            _ => false,
        }
    }

    async fn handle_command(&mut self, command: WsCommand) -> bool {
        match command {
            WsCommand::Push(msg) => self.session.text(msg.as_str()).await.is_err(),
            WsCommand::CheckAlive => {
                if self.last_heartbeat.elapsed() > Self::CLIENT_TIMEOUT {
                    let _ = self
                        .session
                        .clone()
                        .close(Some(CloseCode::Normal.into()))
                        .await;
                    true
                } else {
                    false
                }
            }
            WsCommand::Close => {
                let _ = self
                    .session
                    .clone()
                    .close(Some(CloseCode::Normal.into()))
                    .await;
                true
            }
        }
    }
}

#[derive(Serialize)]
pub struct WsEnvelope<T: Serialize> {
    #[serde(rename = "type")]
    pub msg_type: SyncMessageType,
    pub data: T,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum SyncMessageType {
    // 100 - game
    GameNewAnnouncement = 101,

    // 200 - team
    TeamInfoUpdated = 201,
    TeamDisbanded = 202,
    TeamSelfKicked = 203,
    TeamSelfPromoted = 204,

    // 300 - puzzle
    PuzzleSubmitted = 301,
    PuzzleHintUnlocked = 302,

    // 400 - ticket
    TicketUpdated = 401,
    NotificationUpdated = 402,
}

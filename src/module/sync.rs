use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use actix::{Actor, Addr, Handler, Message};
use actix_web::{HttpRequest, HttpResponse, web::Payload};
use actix_web_actors::ws::{self, CloseCode, ProtocolError, WsResponseBuilder};
use dashmap::DashMap;
use num_enum::IntoPrimitive;
use serde::Serialize;
use serde_json::json;
use serde_repr::Serialize_repr;
use time::OffsetDateTime;

use crate::{
    DbPool, db, error::RbInternalError, model::game::RbJudgeAction,
    serde_helpers::serialize_option_offset_datetime,
};

#[derive(Default)]
pub struct SyncHub {
    users: DashMap<i32, Vec<Addr<WsSession>>>,
}

impl SyncHub {
    pub fn create_ws(
        &self,
        req: HttpRequest,
        stream: Payload,
        user_id: i32,
    ) -> actix_web::Result<HttpResponse> {
        let (addr, resp) =
            WsResponseBuilder::new(WsSession::new(), &req, stream).start_with_addr()?;
        self.users.entry(user_id).or_default().push(addr);
        Ok(resp)
    }

    fn push_user<T: Serialize>(&self, user_id: i32, msg_type: SyncMessageType, data: T) {
        if let Some(addrs) = self.users.get(&user_id) {
            let envelope = WsEnvelope { msg_type, data };
            if let Ok(json) = serde_json::to_string(&envelope) {
                let arc_json = Arc::new(json);
                for addr in addrs.iter() {
                    addr.do_send(WsPush(arc_json.clone()));
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
                    for addr in addrs.iter() {
                        addr.do_send(WsPush(arc_json.clone()));
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

    pub fn cleanup(&self) {
        self.users.retain(|_, addrs| {
            addrs.retain(|addr| addr.connected());
            for addr in addrs.iter() {
                let _ = addr.try_send(WsCheckAlive);
            }
            !addrs.is_empty()
        });
    }

    pub async fn invalidate(&self, user_id: i32) {
        if let Some((_, addrs)) = self.users.remove(&user_id) {
            for addr in addrs.iter() {
                let _ = addr.try_send(WsClose);
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

pub struct WsSession {
    last_heartbeat: Instant,
}

impl WsSession {
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

    fn new() -> Self {
        WsSession {
            last_heartbeat: Instant::now(),
        }
    }

    fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }
}

impl Actor for WsSession {
    type Context = ws::WebsocketContext<Self>;
}

impl actix::StreamHandler<Result<ws::Message, ProtocolError>> for WsSession {
    fn handle(
        &mut self,
        msg: Result<ws::Message, ProtocolError>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.heartbeat();
                ctx.pong(&msg);
            }
            Ok(ws::Message::Pong(_)) => {
                self.heartbeat();
            }
            Ok(ws::Message::Text(_)) => {
                self.heartbeat();
            }
            Ok(ws::Message::Binary(_)) => {
                self.heartbeat();
            }
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
            }
            Err(e) => {
                log::warn!("ws error: {:?}", e);
            }
            _ => {}
        }
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct WsPush(pub Arc<String>);

impl Handler<WsPush> for WsSession {
    type Result = ();

    fn handle(&mut self, msg: WsPush, ctx: &mut Self::Context) -> Self::Result {
        ctx.text(msg.0.as_str());
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct WsCheckAlive;

impl Handler<WsCheckAlive> for WsSession {
    type Result = ();

    fn handle(&mut self, _: WsCheckAlive, ctx: &mut Self::Context) -> Self::Result {
        if self.last_heartbeat.elapsed() > Self::CLIENT_TIMEOUT {
            ctx.close(Some(CloseCode::Normal.into()));
        }
    }
}

#[derive(Message)]
#[rtype(result = "()")]
pub struct WsClose;

impl Handler<WsClose> for WsSession {
    type Result = ();

    fn handle(&mut self, _: WsClose, ctx: &mut Self::Context) -> Self::Result {
        ctx.close(Some(CloseCode::Normal.into()));
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
}

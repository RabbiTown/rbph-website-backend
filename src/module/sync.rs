use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use actix::{Actor, Addr, Handler, Message};
use actix_web::{HttpRequest, HttpResponse, web::Payload};
use actix_web_actors::ws::{self, ProtocolError, WsResponseBuilder};
use dashmap::DashMap;
use num_enum::IntoPrimitive;
use serde::Serialize;
use serde_repr::Serialize_repr;

use crate::{DbPool, db, error::RbInternalError};

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

    pub fn do_push<T: Serialize>(&self, user_id: i32, msg_type: SyncMessageType, data: T) {
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

    pub fn do_push_all<T: Serialize>(&self, users: Vec<i32>, msg_type: SyncMessageType, data: T) {
        let envelope = WsEnvelope { msg_type, data };
        if let Ok(json) = serde_json::to_string(&envelope) {
            let arc_json = Arc::new(json);
            for user_id in users {
                if let Some(addrs) = self.users.get(&user_id) {
                    for addr in addrs.iter() {
                        addr.do_send(WsPush(arc_json.clone()));
                    }
                }
            }
        }
    }

    pub async fn do_push_team<T: Serialize>(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        msg_type: SyncMessageType,
        data: T,
    ) -> Result<(), RbInternalError> {
        let members = db::team::get_member_id(db_pool, team_id).await?;
        self.do_push_all(members, msg_type, data);
        Ok(())
    }

    pub fn cleanup(&self) {
        self.users.retain(|_, addrs| {
            addrs.retain(|addr| addr.connected());

            for addr in addrs.iter() {
                let _ = addr.try_send(CheckAlive);
            }

            !addrs.is_empty()
        });
    }
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
pub struct CheckAlive;

impl Handler<CheckAlive> for WsSession {
    type Result = ();

    fn handle(&mut self, _msg: CheckAlive, ctx: &mut Self::Context) -> Self::Result {
        if self.last_heartbeat.elapsed() > Self::CLIENT_TIMEOUT {
            ctx.close(None);
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
    TeamMemberJoined = 201,
    TeamMemberLeft = 202,
    TeamDisbanded = 203,
    TeamInfoUpdated = 204,
    TeamGameStarted = 205,
    TeamGameFinished = 206,

    // 300 - puzzle
    PuzzleSubmitted = 301,
    PuzzleHintUnlocked = 302,
}

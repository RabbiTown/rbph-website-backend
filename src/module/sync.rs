use std::{
    collections::HashMap,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use actix_web::{HttpRequest, HttpResponse, web::Payload};
use actix_ws::{CloseCode, CloseReason, Message, MessageStream, Session};
use dashmap::DashMap;
use deadpool_redis::redis::{self, AsyncCommands, Script};
use futures_util::StreamExt;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_repr::Serialize_repr;
use time::OffsetDateTime;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::{
    DbPool, db,
    db::{
        puzzle::{CurrencyPenaltyShowData, RbPuzzleTeamStateShowData},
        team::RbCurrencyShowData,
    },
    error::RbInternalError,
    kv::KvStore,
    model::game::RbJudgeAction,
    serde_helpers::serialize_option_offset_datetime,
};

pub const CONNECTION_LIMIT_CLOSE_CODE: u16 = 4008;
const SERVICE_RESTART_CLOSE_CODE: u16 = 1012;
const SLOW_CLIENT_CLOSE_CODE: u16 = 1013;
const SYNC_CHANNEL: &str = "sync:v1";
const CONNECTION_KEY_PREFIX: &str = "ws:v1:connection:";
const CONNECTION_SET_PREFIX: &str = "ws:v1:user:";
const CONNECTION_LEASE_SECONDS: usize = 75;
const COMMAND_QUEUE_CAPACITY: usize = 256;

const REGISTER_CONNECTION_SCRIPT: &str = r#"
local set_key = KEYS[1]
local lease_key = ARGV[1] .. ARGV[2]
local members = redis.call('ZRANGE', set_key, 0, -1)
for _, id in ipairs(members) do
    if redis.call('EXISTS', ARGV[1] .. id) == 0 then
        redis.call('ZREM', set_key, id)
    end
end
local now = redis.call('TIME')
local score = now[1] * 1000000 + now[2]
redis.call('SET', lease_key, ARGV[3], 'EX', ARGV[4])
redis.call('ZADD', set_key, score, ARGV[2])
redis.call('EXPIRE', set_key, tonumber(ARGV[4]) * 2)
members = redis.call('ZRANGE', set_key, 0, -1)
local excess = #members - tonumber(ARGV[5])
local evicted = {}
for index = 1, excess do
    local id = members[index]
    redis.call('ZREM', set_key, id)
    redis.call('DEL', ARGV[1] .. id)
    table.insert(evicted, id)
end
return evicted
"#;

const RENEW_CONNECTIONS_SCRIPT: &str = r#"
local invalid = {}
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[3]) * 2)
for index = 4, #ARGV do
    local key = ARGV[1] .. ARGV[index]
    if redis.call('GET', key) == ARGV[2] then
        redis.call('EXPIRE', key, ARGV[3])
    else
        table.insert(invalid, ARGV[index])
    end
end
return invalid
"#;

const UNREGISTER_CONNECTION_SCRIPT: &str = r#"
local lease_key = ARGV[1] .. ARGV[2]
if redis.call('GET', lease_key) == ARGV[3] then
    redis.call('DEL', lease_key)
end
redis.call('ZREM', KEYS[1], ARGV[2])
return 1
"#;

const TRIM_CONNECTIONS_SCRIPT: &str = r#"
local members = redis.call('ZRANGE', KEYS[1], 0, -1)
for _, id in ipairs(members) do
    if redis.call('EXISTS', ARGV[1] .. id) == 0 then
        redis.call('ZREM', KEYS[1], id)
    end
end
members = redis.call('ZRANGE', KEYS[1], 0, -1)
local excess = #members - tonumber(ARGV[2])
local evicted = {}
for index = 1, excess do
    local id = members[index]
    redis.call('ZREM', KEYS[1], id)
    redis.call('DEL', ARGV[1] .. id)
    table.insert(evicted, id)
end
return evicted
"#;

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SyncTarget {
    Broadcast,
    Users { ids: Vec<i32> },
    Connections { ids: Vec<Uuid> },
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SyncCommand {
    Deliver { message: String },
    CloseConnections { code: u16, reason: String },
}

#[derive(Deserialize, Serialize)]
struct SyncBusEnvelope {
    version: u8,
    event_id: Uuid,
    target: SyncTarget,
    command: SyncCommand,
}

pub struct SyncHub {
    users: DashMap<i32, Vec<WsSessionHandle>>,
    connections: DashMap<Uuid, (i32, WsSessionHandle)>,
    kv: KvStore,
    instance_id: Uuid,
    bus_ready: AtomicBool,
}

impl SyncHub {
    pub fn new(kv: KvStore) -> Self {
        Self {
            users: DashMap::new(),
            connections: DashMap::new(),
            kv,
            instance_id: Uuid::new_v4(),
            bus_ready: AtomicBool::new(false),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.bus_ready.load(Ordering::Acquire)
    }

    pub async fn create_ws(
        self: &Arc<Self>,
        req: HttpRequest,
        stream: Payload,
        user_id: i32,
        max_connections: usize,
    ) -> actix_web::Result<HttpResponse> {
        if !self.is_ready() {
            return Err(actix_web::error::ErrorServiceUnavailable(
                "WebSocket synchronization is unavailable",
            ));
        }

        let connection_id = Uuid::new_v4();
        let (resp, session, stream) = actix_ws::handle(&req, stream)?;
        let evicted = self
            .register_connection(user_id, connection_id, max_connections)
            .await
            .map_err(actix_web::error::ErrorServiceUnavailable)?;
        let (handle, rx) = WsSessionHandle::new(connection_id);
        self.users.entry(user_id).or_default().push(handle.clone());
        self.connections
            .insert(connection_id, (user_id, handle.clone()));

        let hub = self.clone();
        actix_web::rt::spawn(async move {
            WsSession::new(session, stream, rx, handle.close_rx())
                .run()
                .await;
            hub.remove_connection(user_id, connection_id).await;
        });

        if !evicted.is_empty() {
            self.publish_command(
                SyncTarget::Connections { ids: evicted },
                SyncCommand::CloseConnections {
                    code: CONNECTION_LIMIT_CLOSE_CODE,
                    reason: "Connection replaced due to per-user limit".to_string(),
                },
            )
            .await;
        }
        Ok(resp)
    }

    async fn publish_message<T: Serialize>(
        &self,
        target: SyncTarget,
        msg_type: SyncMessageType,
        data: T,
    ) {
        let message = match serde_json::to_string(&WsEnvelope { msg_type, data }) {
            Ok(message) => message,
            Err(error) => {
                log::error!("failed to serialize WebSocket message: {error}");
                return;
            }
        };
        self.publish_command(target, SyncCommand::Deliver { message })
            .await;
    }

    async fn publish_users<T: Serialize>(
        &self,
        mut users: Vec<i32>,
        msg_type: SyncMessageType,
        data: T,
    ) {
        users.sort_unstable();
        users.dedup();
        if users.is_empty() {
            return;
        }
        self.publish_message(SyncTarget::Users { ids: users }, msg_type, data)
            .await;
    }

    async fn publish_team<T: Serialize>(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        msg_type: SyncMessageType,
        data: T,
    ) -> Result<(), RbInternalError> {
        let members = db::team::get_member_id(db_pool, team_id).await?;
        self.publish_users(members, msg_type, data).await;
        Ok(())
    }

    pub async fn notify_game_release_updated(&self, game_id: i32, cursor: i64, force: bool) {
        self.publish_message(
            SyncTarget::Broadcast,
            SyncMessageType::GameReleaseUpdated,
            json!({ "game_id": game_id, "cursor": cursor, "force": force }),
        )
        .await;
    }

    pub async fn notify_game_announcement_updated(&self, game_id: Option<i32>) {
        self.publish_message(
            SyncTarget::Broadcast,
            SyncMessageType::GameNewAnnouncement,
            json!({ "game_id": game_id }),
        )
        .await;
    }

    pub async fn notify_game_frontend_updated(&self, game_id: i32, revision: i64) {
        self.publish_message(
            SyncTarget::Broadcast,
            SyncMessageType::GameFrontendUpdated,
            json!({ "game_id": game_id, "revision": revision }),
        )
        .await;
    }

    pub async fn notify_system_status_updated(&self) {
        self.publish_message(
            SyncTarget::Broadcast,
            SyncMessageType::SystemStatusUpdated,
            (),
        )
        .await;
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
        if event.content_changed {
            sync["content_changed"] = json!(true);
        }
        if event.solved {
            sync["solved"] = json!(true);
            sync["unlocks"] = json!(event.unlocks);
        }

        self.publish_team(
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

        self.publish_team(
            db_pool,
            event.team_id,
            SyncMessageType::PuzzleHintUnlocked,
            sync,
        )
        .await
    }

    pub async fn notify_puzzle_backend_events(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        events: Vec<PuzzleBackendEventSync>,
    ) -> Result<(), RbInternalError> {
        if events.is_empty() {
            return Ok(());
        }

        let members = db::team::get_member_id(db_pool, team_id).await?;
        for event in events {
            self.publish_users(
                members.clone(),
                SyncMessageType::PuzzleBackendEvent,
                json!({
                    "puzzle_id": event.puzzle_id,
                    "event": event.event,
                    "payload": event.payload,
                    "actor": {
                        "id": event.user_id,
                        "nickname": event.user_nickname,
                    },
                    "source": {
                        "type": event.source_type,
                        "function": event.function,
                    },
                }),
            )
            .await;
        }
        Ok(())
    }

    pub async fn notify_team_info_updated(
        &self,
        db_pool: &DbPool,
        team_id: i32,
    ) -> Result<(), RbInternalError> {
        self.publish_team(db_pool, team_id, SyncMessageType::TeamInfoUpdated, ())
            .await
    }

    pub async fn notify_team_disbanded(&self, users: &[i32]) {
        self.publish_users(users.to_vec(), SyncMessageType::TeamDisbanded, ())
            .await;
    }

    pub async fn notify_team_self_kicked(&self, user_id: i32) {
        self.publish_users(vec![user_id], SyncMessageType::TeamSelfKicked, ())
            .await;
    }

    pub async fn notify_team_self_promoted(&self, user_id: i32) {
        self.publish_users(vec![user_id], SyncMessageType::TeamSelfPromoted, ())
            .await;
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

        let members = db::team::get_member_id(db_pool, ticket.team_id).await?;
        let staff_identity = db::game::get_staff_identity(db_pool, ticket.game_id).await?;
        if staff_identity.is_some() && moderators.contains(&actor_id) {
            let moderator_ids = moderators
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            let players = members
                .into_iter()
                .filter(|user_id| !moderator_ids.contains(user_id))
                .collect::<Vec<_>>();
            let mut anonymous_data = data.clone();
            anonymous_data["actor_id"] = json!(db::ticket::STAFF_ALIAS_USER_ID);
            self.publish_users(players, SyncMessageType::TicketUpdated, anonymous_data)
                .await;
            self.publish_users(moderators, SyncMessageType::TicketUpdated, data)
                .await;
        } else {
            let mut recipients = members;
            recipients.extend(moderators);
            self.publish_users(recipients, SyncMessageType::TicketUpdated, data)
                .await;
        }
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
        self.publish_team(
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
        self.publish_team(
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

    async fn publish_command(&self, target: SyncTarget, command: SyncCommand) {
        let payload = match serde_json::to_string(&SyncBusEnvelope {
            version: 1,
            event_id: Uuid::new_v4(),
            target,
            command,
        }) {
            Ok(payload) => payload,
            Err(error) => {
                log::error!("failed to serialize sync bus message: {error}");
                return;
            }
        };
        let mut conn = match self.kv.get().await {
            Ok(conn) => conn,
            Err(error) => {
                log::error!("failed to get Redis connection for sync event: {error}");
                return;
            }
        };
        let result: redis::RedisResult<u64> =
            conn.publish(self.kv.channel(SYNC_CHANNEL), payload).await;
        match result {
            Ok(0) => log::error!("sync event published without any active subscribers"),
            Ok(_) => {}
            Err(error) => log::error!("failed to publish sync event: {error}"),
        }
    }

    fn dispatch(&self, envelope: SyncBusEnvelope) {
        if envelope.version != 1 {
            log::warn!(
                "ignored unsupported sync event version: {}",
                envelope.version
            );
            return;
        }
        let connections: Vec<WsSessionHandle> = match envelope.target {
            SyncTarget::Broadcast => self
                .connections
                .iter()
                .map(|connection| connection.value().1.clone())
                .collect(),
            SyncTarget::Users { mut ids } => {
                ids.sort_unstable();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|user_id| self.users.get(&user_id))
                    .flat_map(|sessions| sessions.iter().cloned().collect::<Vec<_>>())
                    .collect()
            }
            SyncTarget::Connections { mut ids } => {
                ids.sort_unstable();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|connection_id| {
                        self.connections
                            .get(&connection_id)
                            .map(|connection| connection.value().1.clone())
                    })
                    .collect()
            }
        };
        match envelope.command {
            SyncCommand::Deliver { message } => {
                let message = Arc::new(message);
                for connection in connections {
                    let _ = connection.push(message.clone());
                }
            }
            SyncCommand::CloseConnections { code, reason } => {
                let reason: CloseReason = (CloseCode::Other(code), reason).into();
                for connection in connections {
                    connection.close(Some(reason.clone()));
                }
            }
        }
    }

    pub async fn run_subscriber(self: Arc<Self>) {
        let mut retry_seconds = 1_u64;
        loop {
            let result = self.subscribe_once().await;
            let was_ready = self.is_ready();
            self.mark_bus_unavailable();
            if let Err(error) = result {
                log::error!("sync bus subscriber disconnected: {error}");
            }
            if was_ready {
                retry_seconds = 1;
            }
            tokio::time::sleep(Duration::from_secs(retry_seconds)).await;
            retry_seconds = (retry_seconds * 2).min(30);
        }
    }

    async fn subscribe_once(&self) -> Result<(), RbInternalError> {
        let client = self.kv.redis_client()?;
        let mut pubsub = client.get_async_pubsub().await?;
        pubsub.subscribe(self.kv.channel(SYNC_CHANNEL)).await?;
        self.bus_ready.store(true, Ordering::Release);
        log::info!(
            "sync bus subscriber is ready (instance {})",
            self.instance_id
        );

        let mut stream = pubsub.on_message();
        while let Some(message) = stream.next().await {
            let payload = match message.get_payload::<String>() {
                Ok(payload) => payload,
                Err(error) => {
                    log::warn!("ignored invalid sync bus payload: {error}");
                    continue;
                }
            };
            match serde_json::from_str::<SyncBusEnvelope>(&payload) {
                Ok(envelope) => self.dispatch(envelope),
                Err(error) => log::warn!("ignored malformed sync bus message: {error}"),
            }
        }
        Err(RbInternalError::Other(
            "sync bus subscription ended".to_string(),
        ))
    }

    fn mark_bus_unavailable(&self) {
        if !self.bus_ready.swap(false, Ordering::AcqRel) {
            return;
        }
        let reason: CloseReason = (
            CloseCode::Other(SERVICE_RESTART_CLOSE_CODE),
            "WebSocket synchronization is restarting",
        )
            .into();
        for connection in self.connections.iter() {
            connection.value().1.close(Some(reason.clone()));
        }
    }

    async fn register_connection(
        &self,
        user_id: i32,
        connection_id: Uuid,
        max_connections: usize,
    ) -> Result<Vec<Uuid>, RbInternalError> {
        let mut conn = self.kv.get().await?;
        let connection_key_prefix = self.kv.key(CONNECTION_KEY_PREFIX);
        let ids: Vec<String> = Script::new(REGISTER_CONNECTION_SCRIPT)
            .key(
                self.kv
                    .key(format!("{CONNECTION_SET_PREFIX}{user_id}:connections")),
            )
            .arg(connection_key_prefix)
            .arg(connection_id.to_string())
            .arg(self.instance_id.to_string())
            .arg(CONNECTION_LEASE_SECONDS)
            .arg(max_connections.max(1))
            .invoke_async(&mut conn)
            .await?;
        Ok(ids
            .into_iter()
            .filter_map(|id| Uuid::parse_str(&id).ok())
            .collect())
    }

    async fn remove_connection(&self, user_id: i32, connection_id: Uuid) {
        self.connections.remove(&connection_id);
        if let Some(mut sessions) = self.users.get_mut(&user_id) {
            sessions.retain(|session| session.connection_id != connection_id);
            if sessions.is_empty() {
                drop(sessions);
                self.users.remove(&user_id);
            }
        }
        self.unregister_connection(user_id, connection_id).await;
    }

    async fn unregister_connection(&self, user_id: i32, connection_id: Uuid) {
        let Ok(mut conn) = self.kv.get().await else {
            return;
        };
        let result: redis::RedisResult<i32> = Script::new(UNREGISTER_CONNECTION_SCRIPT)
            .key(
                self.kv
                    .key(format!("{CONNECTION_SET_PREFIX}{user_id}:connections")),
            )
            .arg(self.kv.key(CONNECTION_KEY_PREFIX))
            .arg(connection_id.to_string())
            .arg(self.instance_id.to_string())
            .invoke_async(&mut conn)
            .await;
        if let Err(error) = result {
            log::warn!("failed to unregister WebSocket connection: {error}");
        }
    }

    pub async fn cleanup(&self) {
        self.users.retain(|_, sessions| {
            sessions.retain(|session| !session.is_closed());
            for session in sessions.iter() {
                let _ = session.check_alive();
            }
            !sessions.is_empty()
        });

        let mut ids_by_user = HashMap::<i32, Vec<Uuid>>::new();
        for connection in self.connections.iter() {
            ids_by_user
                .entry(connection.value().0)
                .or_default()
                .push(*connection.key());
        }
        for (user_id, ids) in ids_by_user {
            for chunk in ids.chunks(200) {
                let Ok(mut conn) = self.kv.get().await else {
                    break;
                };
                let script = Script::new(RENEW_CONNECTIONS_SCRIPT);
                let mut invocation = script.prepare_invoke();
                invocation
                    .key(
                        self.kv
                            .key(format!("{CONNECTION_SET_PREFIX}{user_id}:connections")),
                    )
                    .arg(self.kv.key(CONNECTION_KEY_PREFIX))
                    .arg(self.instance_id.to_string())
                    .arg(CONNECTION_LEASE_SECONDS);
                for id in chunk {
                    invocation.arg(id.to_string());
                }
                let result: redis::RedisResult<Vec<String>> =
                    invocation.invoke_async(&mut conn).await;
                match result {
                    Ok(invalid) => {
                        for id in invalid
                            .into_iter()
                            .filter_map(|id| Uuid::parse_str(&id).ok())
                        {
                            if let Some(connection) = self.connections.get(&id) {
                                let reason = (
                                    CloseCode::Other(CONNECTION_LIMIT_CLOSE_CODE),
                                    "Connection replaced due to per-user limit",
                                )
                                    .into();
                                connection.value().1.close(Some(reason));
                            }
                        }
                    }
                    Err(error) => log::warn!("failed to renew WebSocket leases: {error}"),
                }
            }
        }
    }

    pub async fn enforce_connection_limit(&self, max_connections: usize) {
        let user_ids = self
            .users
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        let mut evicted = Vec::new();
        for user_id in user_ids {
            let Ok(mut conn) = self.kv.get().await else {
                break;
            };
            let result: redis::RedisResult<Vec<String>> = Script::new(TRIM_CONNECTIONS_SCRIPT)
                .key(
                    self.kv
                        .key(format!("{CONNECTION_SET_PREFIX}{user_id}:connections")),
                )
                .arg(self.kv.key(CONNECTION_KEY_PREFIX))
                .arg(max_connections.max(1))
                .invoke_async(&mut conn)
                .await;
            match result {
                Ok(ids) => {
                    evicted.extend(ids.into_iter().filter_map(|id| Uuid::parse_str(&id).ok()))
                }
                Err(error) => log::warn!("failed to enforce WebSocket connection limit: {error}"),
            }
        }
        evicted.sort_unstable();
        evicted.dedup();
        if !evicted.is_empty() {
            self.publish_command(
                SyncTarget::Connections { ids: evicted },
                SyncCommand::CloseConnections {
                    code: CONNECTION_LIMIT_CLOSE_CODE,
                    reason: "Connection replaced due to per-user limit".to_string(),
                },
            )
            .await;
        }
    }

    pub async fn invalidate(&self, user_id: i32) {
        if let Some(sessions) = self.users.get(&user_id) {
            for session in sessions.iter() {
                session.close(Some(CloseCode::Normal.into()));
            }
        }
    }

    pub async fn shutdown(&self) {
        self.bus_ready.store(false, Ordering::Release);
        let connections = self
            .connections
            .iter()
            .map(|entry| (*entry.key(), entry.value().0, entry.value().1.clone()))
            .collect::<Vec<_>>();
        for (_, _, handle) in &connections {
            handle.close(Some(CloseCode::Away.into()));
        }
        for (connection_id, user_id, _) in connections {
            self.unregister_connection(user_id, connection_id).await;
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
    pub content_changed: bool,
    pub sid: Option<String>,
}

pub struct PuzzleHintUnlockedSync {
    pub team_id: i32,
    pub user_id: i32,
    pub hint_id: i32,
    pub sid: Option<String>,
}

#[derive(Clone)]
pub struct PuzzleBackendEventSync {
    pub puzzle_id: i32,
    pub user_id: i32,
    pub user_nickname: String,
    pub event: String,
    pub payload: serde_json::Value,
    pub source_type: &'static str,
    pub function: String,
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
    connection_id: Uuid,
    tx: mpsc::Sender<WsCommand>,
    close_tx: watch::Sender<Option<CloseReason>>,
}

impl WsSessionHandle {
    fn new(connection_id: Uuid) -> (Self, mpsc::Receiver<WsCommand>) {
        let (tx, rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let (close_tx, _) = watch::channel(None);
        (
            Self {
                connection_id,
                tx,
                close_tx,
            },
            rx,
        )
    }

    fn close_rx(&self) -> watch::Receiver<Option<CloseReason>> {
        self.close_tx.subscribe()
    }

    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    fn push(&self, msg: Arc<String>) -> Result<(), ()> {
        match self.tx.try_send(WsCommand::Push(msg)) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.close(Some(
                    (
                        CloseCode::Other(SLOW_CLIENT_CLOSE_CODE),
                        "WebSocket client is too slow",
                    )
                        .into(),
                ));
                Err(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
        }
    }

    fn check_alive(&self) -> Result<(), ()> {
        self.tx.try_send(WsCommand::CheckAlive).map_err(|_| ())
    }

    fn close(&self, reason: Option<CloseReason>) {
        self.close_tx.send_replace(reason);
    }
}

enum WsCommand {
    Push(Arc<String>),
    CheckAlive,
}

pub struct WsSession {
    session: Session,
    stream: MessageStream,
    commands: mpsc::Receiver<WsCommand>,
    close_requests: watch::Receiver<Option<CloseReason>>,
    last_heartbeat: Instant,
}

impl WsSession {
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

    fn new(
        session: Session,
        stream: MessageStream,
        commands: mpsc::Receiver<WsCommand>,
        close_requests: watch::Receiver<Option<CloseReason>>,
    ) -> Self {
        WsSession {
            session,
            stream,
            commands,
            close_requests,
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
                changed = self.close_requests.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let reason = self.close_requests.borrow().clone();
                    let _ = self.session.clone().close(reason).await;
                    break;
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
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, atomic::Ordering},
        time::Duration,
    };

    use deadpool_redis::redis::AsyncCommands;
    use tokio::sync::mpsc::error::TryRecvError;
    use uuid::Uuid;

    use crate::kv::KvStore;

    use super::{
        COMMAND_QUEUE_CAPACITY, CONNECTION_LIMIT_CLOSE_CODE, SLOW_CLIENT_CLOSE_CODE,
        SyncBusEnvelope, SyncCommand, SyncHub, SyncTarget, WsCommand, WsSessionHandle,
    };

    fn test_hub() -> SyncHub {
        let kv = deadpool_redis::Config::from_url("redis://127.0.0.1/15")
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        SyncHub::new(KvStore::new(kv, "redis://127.0.0.1/15", "test"))
    }

    #[test]
    fn slow_connection_is_closed_when_queue_is_full() {
        let (handle, _commands) = WsSessionHandle::new(Uuid::new_v4());
        let close = handle.close_rx();

        for _ in 0..COMMAND_QUEUE_CAPACITY {
            assert!(handle.push(Arc::new("message".to_string())).is_ok());
        }
        assert!(handle.push(Arc::new("overflow".to_string())).is_err());
        let reason = close.borrow().clone().expect("close reason should be set");
        assert_eq!(u16::from(reason.code), SLOW_CLIENT_CLOSE_CODE);
    }

    #[test]
    fn targeted_delivery_is_local_and_deduplicated() {
        let hub = test_hub();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let (first, mut first_rx) = WsSessionHandle::new(first_id);
        let (second, mut second_rx) = WsSessionHandle::new(second_id);
        hub.users.entry(7).or_default().push(first.clone());
        hub.users.entry(8).or_default().push(second.clone());
        hub.connections.insert(first_id, (7, first));
        hub.connections.insert(second_id, (8, second));

        hub.dispatch(SyncBusEnvelope {
            version: 1,
            event_id: Uuid::new_v4(),
            target: SyncTarget::Users { ids: vec![7, 7] },
            command: SyncCommand::Deliver {
                message: "message".to_string(),
            },
        });

        assert!(
            matches!(first_rx.try_recv(), Ok(WsCommand::Push(message)) if message.as_str() == "message")
        );
        assert!(matches!(first_rx.try_recv(), Err(TryRecvError::Empty)));
        assert!(matches!(second_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn unsupported_bus_version_is_ignored() {
        let hub = test_hub();
        let connection_id = Uuid::new_v4();
        let (handle, mut commands) = WsSessionHandle::new(connection_id);
        hub.connections.insert(connection_id, (7, handle));

        hub.dispatch(SyncBusEnvelope {
            version: 2,
            event_id: Uuid::new_v4(),
            target: SyncTarget::Broadcast,
            command: SyncCommand::Deliver {
                message: "message".to_string(),
            },
        });

        assert!(matches!(commands.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn connection_target_closes_only_selected_connections() {
        let hub = test_hub();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let (first, _first_commands) = WsSessionHandle::new(first_id);
        let (second, _second_commands) = WsSessionHandle::new(second_id);
        let first_close = first.close_rx();
        let second_close = second.close_rx();
        hub.connections.insert(first_id, (7, first));
        hub.connections.insert(second_id, (7, second));

        hub.dispatch(SyncBusEnvelope {
            version: 1,
            event_id: Uuid::new_v4(),
            target: SyncTarget::Connections {
                ids: vec![first_id, first_id],
            },
            command: SyncCommand::CloseConnections {
                code: CONNECTION_LIMIT_CLOSE_CODE,
                reason: "connection limit".to_string(),
            },
        });

        let reason = first_close
            .borrow()
            .clone()
            .expect("selected connection should be closed");
        assert_eq!(u16::from(reason.code), CONNECTION_LIMIT_CLOSE_CODE);
        assert!(second_close.borrow().is_none());
    }

    #[tokio::test]
    #[ignore = "requires RBPH_TEST_REDIS_URL"]
    async fn redis_registration_enforces_global_limit_and_lease_eviction() {
        let redis_url = std::env::var("RBPH_TEST_REDIS_URL")
            .expect("RBPH_TEST_REDIS_URL must be set for ignored Redis integration tests");
        let pool = deadpool_redis::Config::from_url(&redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        let kv = KvStore::new(pool.clone(), redis_url, "sync-registration-test");
        let first_hub = SyncHub::new(kv.clone());
        let second_hub = SyncHub::new(kv);
        let user_id = 1_500_000_000 + i32::from(Uuid::new_v4().as_bytes()[0]);
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let third_id = Uuid::new_v4();
        let (first_handle, _commands) = WsSessionHandle::new(first_id);
        let first_close = first_handle.close_rx();
        first_hub
            .users
            .entry(user_id)
            .or_default()
            .push(first_handle.clone());
        first_hub
            .connections
            .insert(first_id, (user_id, first_handle));

        assert!(
            first_hub
                .register_connection(user_id, first_id, 2)
                .await
                .expect("first registration should succeed")
                .is_empty()
        );
        assert!(
            second_hub
                .register_connection(user_id, second_id, 2)
                .await
                .expect("second registration should succeed")
                .is_empty()
        );
        assert_eq!(
            second_hub
                .register_connection(user_id, third_id, 2)
                .await
                .expect("third registration should succeed"),
            vec![first_id]
        );

        first_hub.cleanup().await;
        let reason = first_close
            .borrow()
            .clone()
            .expect("evicted connection should be closed during lease renewal");
        assert_eq!(u16::from(reason.code), super::CONNECTION_LIMIT_CLOSE_CODE);

        let mut conn = pool
            .get()
            .await
            .expect("test Redis should remain available");
        let keys = vec![
            first_hub.kv.key(format!(
                "{}{user_id}:connections",
                super::CONNECTION_SET_PREFIX
            )),
            first_hub
                .kv
                .key(format!("{}{first_id}", super::CONNECTION_KEY_PREFIX)),
            first_hub
                .kv
                .key(format!("{}{second_id}", super::CONNECTION_KEY_PREFIX)),
            first_hub
                .kv
                .key(format!("{}{third_id}", super::CONNECTION_KEY_PREFIX)),
        ];
        let _: deadpool_redis::redis::RedisResult<usize> = conn.del(&keys).await;
    }

    #[tokio::test]
    #[ignore = "requires RBPH_TEST_REDIS_URL"]
    async fn redis_bus_delivers_between_instances_once() {
        let redis_url = std::env::var("RBPH_TEST_REDIS_URL")
            .expect("RBPH_TEST_REDIS_URL must be set for ignored Redis integration tests");
        let pool = deadpool_redis::Config::from_url(&redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        let kv = KvStore::new(pool, redis_url, "sync-bus-test");
        let first_hub = Arc::new(SyncHub::new(kv.clone()));
        let second_hub = Arc::new(SyncHub::new(kv));
        let user_id = 1_600_000_000 + i32::from(Uuid::new_v4().as_bytes()[0]);
        let connection_id = Uuid::new_v4();
        let (handle, mut commands) = WsSessionHandle::new(connection_id);
        let mut close = handle.close_rx();
        second_hub
            .users
            .entry(user_id)
            .or_default()
            .push(handle.clone());
        second_hub
            .connections
            .insert(connection_id, (user_id, handle));

        let first_subscriber = tokio::spawn(first_hub.clone().run_subscriber());
        let second_subscriber = tokio::spawn(second_hub.clone().run_subscriber());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !first_hub.is_ready() || !second_hub.is_ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both sync subscribers should become ready");

        first_hub
            .publish_message(
                SyncTarget::Users { ids: vec![user_id] },
                super::SyncMessageType::TeamInfoUpdated,
                (),
            )
            .await;
        let message = tokio::time::timeout(Duration::from_secs(2), commands.recv())
            .await
            .expect("remote event should arrive")
            .expect("connection command channel should remain open");
        assert!(matches!(message, WsCommand::Push(_)));
        assert!(matches!(commands.try_recv(), Err(TryRecvError::Empty)));

        first_hub
            .publish_command(
                SyncTarget::Connections {
                    ids: vec![connection_id],
                },
                SyncCommand::CloseConnections {
                    code: CONNECTION_LIMIT_CLOSE_CODE,
                    reason: "connection limit".to_string(),
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(2), close.changed())
            .await
            .expect("remote close command should arrive")
            .expect("connection close channel should remain open");
        let reason = close
            .borrow()
            .clone()
            .expect("remote target connection should be closed");
        assert_eq!(u16::from(reason.code), CONNECTION_LIMIT_CLOSE_CODE);

        first_subscriber.abort();
        second_subscriber.abort();
    }

    #[tokio::test]
    #[ignore = "requires RBPH_TEST_REDIS_URL"]
    async fn redis_bus_is_isolated_between_deployments() {
        let redis_url = std::env::var("RBPH_TEST_REDIS_URL")
            .expect("RBPH_TEST_REDIS_URL must be set for ignored Redis integration tests");
        let pool = deadpool_redis::Config::from_url(&redis_url)
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .expect("test Redis pool configuration should be valid");
        let random = Uuid::new_v4().simple().to_string();
        let suffix = &random[..8];
        let first_hub = Arc::new(SyncHub::new(KvStore::new(
            pool.clone(),
            redis_url.clone(),
            &format!("test-a-{suffix}"),
        )));
        let second_hub = Arc::new(SyncHub::new(KvStore::new(
            pool,
            redis_url,
            &format!("test-b-{suffix}"),
        )));
        let user_id = 1_700_000_000 + i32::from(Uuid::new_v4().as_bytes()[0]);
        let connection_id = Uuid::new_v4();
        let (handle, mut commands) = WsSessionHandle::new(connection_id);
        second_hub
            .users
            .entry(user_id)
            .or_default()
            .push(handle.clone());
        second_hub
            .connections
            .insert(connection_id, (user_id, handle));

        let first_subscriber = tokio::spawn(first_hub.clone().run_subscriber());
        let second_subscriber = tokio::spawn(second_hub.clone().run_subscriber());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !first_hub.is_ready() || !second_hub.is_ready() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both sync subscribers should become ready");

        first_hub
            .publish_message(
                SyncTarget::Users { ids: vec![user_id] },
                super::SyncMessageType::TeamInfoUpdated,
                (),
            )
            .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), commands.recv())
                .await
                .is_err()
        );

        first_subscriber.abort();
        second_subscriber.abort();
    }

    #[test]
    fn bus_failure_marks_instance_unready_and_closes_connections() {
        let hub = test_hub();
        let connection_id = Uuid::new_v4();
        let (handle, _commands) = WsSessionHandle::new(connection_id);
        let close = handle.close_rx();
        hub.connections.insert(connection_id, (7, handle));
        hub.bus_ready.store(true, Ordering::Release);

        hub.mark_bus_unavailable();

        assert!(!hub.is_ready());
        let reason = close.borrow().clone().expect("connection should be closed");
        assert_eq!(u16::from(reason.code), super::SERVICE_RESTART_CLOSE_CODE);
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
    // 0 - system
    SystemStatusUpdated = 1,

    // 100 - game
    GameNewAnnouncement = 101,
    GameReleaseUpdated = 102,
    GameFrontendUpdated = 103,

    // 200 - team
    TeamInfoUpdated = 201,
    TeamDisbanded = 202,
    TeamSelfKicked = 203,
    TeamSelfPromoted = 204,

    // 300 - puzzle
    PuzzleSubmitted = 301,
    PuzzleHintUnlocked = 302,
    PuzzleBackendEvent = 303,

    // 400 - ticket
    TicketUpdated = 401,
    NotificationUpdated = 402,
}

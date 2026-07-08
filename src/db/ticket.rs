use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_repr::Serialize_repr;
use sqlx::PgConnection;
use time::OffsetDateTime;

use crate::{
    AppState, DbPool,
    db::{round::RbRoundSimpleData, team::RbCurrencyShowData},
    error::RbInternalError,
    model::{
        game::{RbContentType, RbTeamPuzzleState, RbTeamState, RbTicketSenderType, RbTicketState},
        user::RbUserRole,
    },
};

#[derive(Clone)]
pub struct TicketUserInfo {
    pub ticket_id: i32,
    pub state: RbTicketState,
    pub member_access: bool,
    pub mod_access: bool,
    pub admin_access: bool,
}

pub async fn get_ticket_user_info(
    db_pool: &DbPool,
    ticket_id: i32,
    user_id: i32,
) -> Result<Option<TicketUserInfo>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT t.state, u.urole,
            EXISTS (
                SELECT 1
                FROM rb_team_member tm
                WHERE tm.team_id = t.team_id
                AND tm.user_id = u.id
            ) AS is_member
        FROM rb_ticket t
        JOIN rb_user u ON u.id = $2
        WHERE t.id = $1;",
        ticket_id,
        user_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result.map(|x| TicketUserInfo {
        ticket_id,
        state: x.state.into(),
        member_access: x.is_member.unwrap_or(false),
        mod_access: RbUserRole::from(x.urole).is_moderator(),
        admin_access: RbUserRole::from(x.urole).is_admin(),
    }))
}

#[derive(Serialize)]
pub struct TicketAggreInfoTeam {
    id: i32,
    name: String,
    state: RbTeamState,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    currency: Vec<RbCurrencyShowData>,
}

#[derive(Serialize)]
pub struct TicketAggreInfoPuzzle {
    id: i32,
    slug: Option<String>,
    title: String,
    state: RbTeamPuzzleState,
    round: RbRoundSimpleData,
}

#[derive(Clone, Serialize)]
pub struct TicketAggreInfoUser {
    pub id: i32,
    pub nickname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

fn user_avatar(email: Option<&str>, provider: Option<i16>) -> Option<String> {
    email.map(|email| {
        crate::model::user::avatar_url(
            email,
            crate::model::user::AvatarProvider::try_from(provider.unwrap_or_default())
                .unwrap_or_default(),
        )
    })
}

#[derive(Serialize)]
pub struct TicketMessage {
    r#type: TicketThreadItemType,
    pub id: i32,
    sender: TicketAggreInfoUser,
    sender_type: RbTicketSenderType,
    cost_id: Option<i32>,
    cost_amount: i64,
    unlocked: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<RbContentType>,

    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    ctime_at: OffsetDateTime,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    utime_at: Option<OffsetDateTime>,
}

impl TicketMessage {
    pub fn id(&self) -> i32 {
        self.id
    }
}

#[repr(i16)]
#[derive(Serialize_repr)]
pub enum TicketThreadItemType {
    Message = 0,
    Operation = 1,
}

#[repr(i16)]
#[derive(IntoPrimitive, Serialize_repr)]
pub enum TicketOperationAction {
    Open = 1,
    Close = 2,
    AutoCloseSolved = 3,
}

impl From<i16> for TicketOperationAction {
    fn from(value: i16) -> Self {
        match value {
            1 => Self::Open,
            2 => Self::Close,
            3 => Self::AutoCloseSolved,
            _ => Self::Close,
        }
    }
}

#[derive(Serialize)]
pub struct TicketOperation {
    r#type: TicketThreadItemType,
    id: i32,
    action: TicketOperationAction,
    actor: TicketAggreInfoUser,
    actor_type: RbTicketSenderType,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<TicketMessage>,

    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    ctime_at: OffsetDateTime,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum TicketThreadItem {
    Message(TicketMessage),
    Operation(TicketOperation),
}

impl TicketThreadItem {
    fn ctime_at(&self) -> OffsetDateTime {
        match self {
            Self::Message(message) => message.ctime_at,
            Self::Operation(operation) => operation.ctime_at,
        }
    }

    fn id(&self) -> i32 {
        match self {
            Self::Message(message) => message.id,
            Self::Operation(operation) => operation.id,
        }
    }

    fn same_time_rank(&self) -> i8 {
        match self {
            Self::Operation(operation) => match operation.action {
                TicketOperationAction::Open => 0,
                TicketOperationAction::Close | TicketOperationAction::AutoCloseSolved => 2,
            },
            Self::Message(_) => 1,
        }
    }

    pub fn message_id(&self) -> Option<i32> {
        match self {
            Self::Message(message) => Some(message.id()),
            Self::Operation(operation) => operation.message.as_ref().map(TicketMessage::id),
        }
    }
}

#[derive(Serialize)]
pub struct TicketSummary {
    pub id: i32,
    state: RbTicketState,
    #[serde(skip_serializing_if = "Option::is_none")]
    game_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    team: Option<TicketAggreInfoTeam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    puzzle: Option<TicketAggreInfoPuzzle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_count: Option<i64>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    last_at: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_by: Option<RbTicketSenderType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee: Option<TicketAggreInfoUser>,
}

impl TicketSummary {
    pub fn id(&self) -> i32 {
        self.id
    }

    pub fn game_id(&self) -> Option<i32> {
        self.game_id
    }

    pub fn is_puzzle_ticket(&self) -> bool {
        self.puzzle.is_some()
    }

    pub fn currency_ids(&self) -> Vec<i32> {
        self.team
            .as_ref()
            .map(|team| team.currency.iter().map(|currency| currency.id).collect())
            .unwrap_or_default()
    }

    pub fn hide_assignee(&mut self) {
        self.assignee = None;
    }
}

#[derive(Clone, Serialize)]
pub struct TicketPerm {
    send_block: TicketSendBlock,
    can_host: bool,
    can_view_locked: bool,
    content_type: Vec<RbContentType>,
    currency: Vec<i32>,
}

impl TicketPerm {
    pub fn new(
        can_host: bool,
        can_view_locked: bool,
        can_use_trusted_content: bool,
        currency: Vec<i32>,
        send_block: TicketSendBlock,
    ) -> Self {
        let content_type = if can_use_trusted_content {
            vec![
                RbContentType::Markdown,
                RbContentType::Html,
                RbContentType::UnsafeMarkdown,
            ]
        } else {
            vec![RbContentType::UnsafeMarkdown]
        };

        Self {
            send_block,
            can_host,
            can_view_locked,
            content_type,
            currency,
        }
    }
}

#[repr(i16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize_repr)]
pub enum TicketSendBlock {
    Ok = 0,
    NoAccess = -1,
    Closed = -2,
    Pending = -3,
    FeatureClosed = -4,
    FeatureExistingOnly = -5,
    TeamFeatureBanned = -6,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlayerFeatureAccess {
    Open,
    ExistingOnly,
    GameClosed,
    TeamFeatureBanned,
}

impl PlayerFeatureAccess {
    fn from_value(value: i16) -> Self {
        match value {
            2 => Self::Open,
            1 => Self::ExistingOnly,
            -2 => Self::TeamFeatureBanned,
            _ => Self::GameClosed,
        }
    }

    pub fn send_block(self, has_existing: bool) -> TicketSendBlock {
        match self {
            Self::Open => TicketSendBlock::Ok,
            Self::ExistingOnly if has_existing => TicketSendBlock::Ok,
            Self::ExistingOnly => TicketSendBlock::FeatureExistingOnly,
            Self::GameClosed => TicketSendBlock::FeatureClosed,
            Self::TeamFeatureBanned => TicketSendBlock::TeamFeatureBanned,
        }
    }
}

#[derive(Serialize)]
pub struct TicketThread {
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket: Option<TicketSummary>,
    messages: Vec<TicketThreadItem>,
    history: TicketHistory,
    perm: TicketPerm,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TicketCursor {
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    at: OffsetDateTime,
    rank: i8,
    id: i32,
}

impl TicketCursor {
    pub fn decode(value: &str) -> Option<Self> {
        URL_SAFE_NO_PAD
            .decode(value)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("ticket cursor serialization"))
    }
}

#[derive(Default)]
pub struct TicketPageRequest {
    pub before: Option<TicketCursor>,
    pub after: Option<TicketCursor>,
    pub stop: Option<TicketCursor>,
    pub(crate) oldest: bool,
}

#[derive(Default, Serialize)]
pub struct TicketHistory {
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newer: Option<String>,
    has_more: bool,
}

struct TicketMessagePage {
    items: Vec<TicketThreadItem>,
    history: TicketHistory,
}

impl TicketThread {
    pub fn ticket(&self) -> Option<&TicketSummary> {
        self.ticket.as_ref()
    }

    pub fn messages(&self) -> &[TicketThreadItem] {
        &self.messages
    }

    pub fn perm(&self) -> &TicketPerm {
        &self.perm
    }
}

async fn get_pending_count(db_pool: &DbPool, ticket_id: i32) -> Result<i64, RbInternalError> {
    let pending = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rb_message m
        WHERE ticket_id = $1
        AND sender_type = 0
        AND NOT EXISTS (
            SELECT 1
            FROM rb_message AS reply
            WHERE reply.ticket_id = m.ticket_id
                AND reply.sender_type = 1
                AND reply.id > m.id
        );",
        ticket_id
    )
    .fetch_one(db_pool)
    .await?
    .unwrap_or(0);

    Ok(pending)
}

pub async fn calc_send_block(
    db_pool: &DbPool,
    ticket_id: i32,
    state: RbTicketState,
    can_send: bool,
    max_pending: Option<i64>,
) -> Result<TicketSendBlock, RbInternalError> {
    if !can_send {
        return Ok(TicketSendBlock::NoAccess);
    }
    if matches!(state, RbTicketState::Closed) {
        return Ok(TicketSendBlock::Closed);
    }
    if let Some(max_pending) = max_pending
        && get_pending_count(db_pool, ticket_id).await? >= max_pending
    {
        return Ok(TicketSendBlock::Pending);
    }

    Ok(TicketSendBlock::Ok)
}

#[derive(Serialize)]
pub struct TicketPuzzleList {
    open_block: TicketOpenBlock,
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_helpers::serialize_option_offset_datetime"
    )]
    cooldown_till: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_tickets: Vec<TicketOpenInfo>,
    tickets: Vec<TicketSummary>,
}

#[repr(i16)]
#[derive(Clone, Copy, Serialize_repr)]
pub enum TicketOpenBlock {
    Ok = 0,
    CurrentPuzzlePending = -1,
    PendingLimit = -2,
    Cooldown = -3,
    Disabled = -4,
    FeatureClosed = -5,
    FeatureExistingOnly = -6,
    TeamFeatureBanned = -7,
}

pub async fn player_ticket_feature_access(
    db_pool: &DbPool,
    ticket_id: i32,
) -> Result<PlayerFeatureAccess, RbInternalError> {
    let value = sqlx::query_scalar!(
        "SELECT (CASE
            WHEN NOT COALESCE(tf.enabled, TRUE) THEN -2
            WHEN COALESCE(gf.state, 2) = 0 THEN 0
            WHEN NOT (tk.puzzle_id IS NULL OR EXISTS (
                SELECT 1 FROM rb_puzzle p
                JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
                WHERE p.id = tk.puzzle_id AND rp.release_at <= NOW()
            )) THEN 0
            ELSE COALESCE(gf.state, 2)
        END)::SMALLINT AS \"access!\"
        FROM rb_ticket tk
        JOIN rb_team t ON t.id = tk.team_id
        LEFT JOIN rb_game_feature gf ON gf.game_id = t.game_id
            AND gf.feature_type = CASE WHEN tk.puzzle_id IS NULL THEN 1 ELSE 2 END
        LEFT JOIN rb_team_feature tf ON tf.team_id = t.id
            AND tf.feature_type = CASE WHEN tk.puzzle_id IS NULL THEN 1 ELSE 2 END
        WHERE tk.id = $1;",
        ticket_id
    )
    .fetch_optional(db_pool)
    .await?
    .unwrap_or(0);
    Ok(PlayerFeatureAccess::from_value(value))
}

pub async fn player_dm_feature_access(
    db_pool: &DbPool,
    team_id: i32,
) -> Result<PlayerFeatureAccess, RbInternalError> {
    let value = sqlx::query_scalar!(
        "SELECT (CASE
            WHEN NOT COALESCE(tf.enabled, TRUE) THEN -2
            ELSE COALESCE(gf.state, 2)
        END)::SMALLINT AS \"access!\"
        FROM rb_team t
        LEFT JOIN rb_game_feature gf ON gf.game_id = t.game_id AND gf.feature_type = 1
        LEFT JOIN rb_team_feature tf ON tf.team_id = t.id AND tf.feature_type = 1
        WHERE t.id = $1;",
        team_id
    )
    .fetch_optional(db_pool)
    .await?
    .unwrap_or(0);
    Ok(PlayerFeatureAccess::from_value(value))
}

#[derive(Serialize)]
pub struct TicketOpenInfo {
    id: i32,
    puzzle_id: i32,
    puzzle_title: String,
}

#[allow(clippy::too_many_arguments)]
fn make_message(
    id: Option<i32>,
    sender_id: Option<i32>,
    sender_nickname: Option<String>,
    sender_type: Option<i16>,
    cost_id: Option<i32>,
    cost_amount: Option<i64>,
    unlocked: Option<bool>,
    content: Option<String>,
    content_type: Option<i16>,
    ctime_at: Option<OffsetDateTime>,
    utime_at: Option<OffsetDateTime>,
) -> Option<TicketMessage> {
    Some(TicketMessage {
        r#type: TicketThreadItemType::Message,
        id: id?,
        sender: TicketAggreInfoUser {
            id: sender_id?,
            nickname: sender_nickname?,
            avatar: None,
            email: None,
        },
        sender_type: RbTicketSenderType::from_primitive(sender_type?),
        cost_id,
        cost_amount: cost_amount?,
        unlocked: unlocked?,
        content,
        content_type: content_type.map(RbContentType::from_primitive),
        ctime_at: ctime_at?,
        utime_at,
    })
}

fn item_cursor(item: &TicketThreadItem) -> TicketCursor {
    TicketCursor {
        at: item.ctime_at(),
        rank: item.same_time_rank(),
        id: item.id(),
    }
}

async fn get_ticket_messages_page(
    db_pool: &DbPool,
    ticket_id: i32,
    can_view_locked: bool,
    request: &TicketPageRequest,
    limit: usize,
) -> Result<TicketMessagePage, RbInternalError> {
    let mut items: Vec<TicketThreadItem> = Vec::new();
    let after = request.after.is_some() || request.oldest;
    let cursor = request.after.as_ref().or(request.before.as_ref());
    let cursor_at = cursor.map(|value| value.at);
    let cursor_rank = cursor.map(|value| value.rank).unwrap_or(0);
    let cursor_id = cursor.map(|value| value.id).unwrap_or(0);
    let stop_at = request.stop.as_ref().map(|value| value.at);
    let stop_rank = request.stop.as_ref().map(|value| value.rank).unwrap_or(0);
    let stop_id = request.stop.as_ref().map(|value| value.id).unwrap_or(0);
    let query_limit = (limit + 1) as i64;

    let result = sqlx::query!(
        "SELECT m.id, m.sender, m.sender_type, m.cost_id, m.cost_amount,
                m.unlocked, m.ctime_at, m.utime_at,
                u.id AS u_id, u.nickname AS u_nickname,
                CASE WHEN ($2 OR m.unlocked) THEN m.content ELSE NULL END AS content,
                CASE WHEN ($2 OR m.unlocked) THEN m.content_type ELSE NULL END AS content_type
        FROM rb_message m
        JOIN rb_user u ON u.id = m.sender
        WHERE ticket_id = $1
        AND NOT EXISTS (
            SELECT 1
            FROM rb_ticket_operation o
            WHERE o.message_id = m.id
        )
        AND ($3::TIMESTAMPTZ IS NULL OR
            ($4 AND (m.ctime_at > $3 OR (m.ctime_at = $3 AND (1 > $5 OR (1 = $5 AND m.id > $6))))) OR
            (NOT $4 AND (m.ctime_at < $3 OR (m.ctime_at = $3 AND (1 < $5 OR (1 = $5 AND m.id < $6))))))
        AND ($7::TIMESTAMPTZ IS NULL OR m.ctime_at < $7 OR
            (m.ctime_at = $7 AND (1 < $8 OR (1 = $8 AND m.id < $9))))
        ORDER BY
            CASE WHEN $4 THEN m.ctime_at END ASC,
            CASE WHEN NOT $4 THEN m.ctime_at END DESC,
            CASE WHEN $4 THEN m.id END ASC,
            CASE WHEN NOT $4 THEN m.id END DESC
        LIMIT $10",
        ticket_id,
        can_view_locked,
        cursor_at,
        after,
        i32::from(cursor_rank),
        cursor_id,
        stop_at,
        i32::from(stop_rank),
        stop_id,
        query_limit,
    )
    .fetch_all(db_pool)
    .await?
    .into_iter()
    .map(|x| {
        TicketThreadItem::Message(TicketMessage {
            r#type: TicketThreadItemType::Message,
            id: x.id,
            sender: TicketAggreInfoUser {
                id: x.u_id,
                nickname: x.u_nickname,
                avatar: None,
                email: None,
            },
            sender_type: RbTicketSenderType::from_primitive(x.sender_type),
            cost_id: x.cost_id,
            cost_amount: x.cost_amount,
            unlocked: x.unlocked,

            content: x.content,
            content_type: x.content_type.map(RbContentType::from_primitive),
            ctime_at: x.ctime_at,
            utime_at: x.utime_at,
        })
    })
    .collect::<Vec<_>>();

    items.extend(result);

    let operations = sqlx::query!(
        "SELECT o.id, o.action, o.actor_type, o.ctime_at, u.id AS u_id, u.nickname AS u_nickname,
            m.id AS \"m_id?\", m.sender_type AS \"m_st?\",
            m.cost_id AS \"m_ci?\", m.cost_amount AS \"m_ca?\",
            m.unlocked AS \"m_ul?\", m.ctime_at AS \"m_c?\", m.utime_at AS \"m_u?\",
            mu.id AS \"mu_id?\", mu.nickname AS \"mu_n?\",
            CASE WHEN ($2 OR m.unlocked) THEN m.content ELSE NULL END AS \"m_t?\",
            CASE WHEN ($2 OR m.unlocked) THEN m.content_type ELSE NULL END AS \"m_ct?\"
        FROM rb_ticket_operation o
        JOIN rb_user u ON u.id = o.actor
        LEFT JOIN rb_message m ON m.id = o.message_id
        LEFT JOIN rb_user mu ON mu.id = m.sender
        WHERE o.ticket_id = $1
        AND ($3::TIMESTAMPTZ IS NULL OR
            ($4 AND (o.ctime_at > $3 OR (o.ctime_at = $3 AND
                ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) > $5 OR
                ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) = $5 AND o.id > $6))))) OR
            (NOT $4 AND (o.ctime_at < $3 OR (o.ctime_at = $3 AND
                ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) < $5 OR
                ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) = $5 AND o.id < $6))))))
        AND ($7::TIMESTAMPTZ IS NULL OR o.ctime_at < $7 OR (o.ctime_at = $7 AND
            ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) < $8 OR
            ((CASE WHEN o.action = 1 THEN 0 ELSE 2 END) = $8 AND o.id < $9))))
        ORDER BY
            CASE WHEN $4 THEN o.ctime_at END ASC,
            CASE WHEN NOT $4 THEN o.ctime_at END DESC,
            CASE WHEN $4 THEN (CASE WHEN o.action = 1 THEN 0 ELSE 2 END) END ASC,
            CASE WHEN NOT $4 THEN (CASE WHEN o.action = 1 THEN 0 ELSE 2 END) END DESC,
            CASE WHEN $4 THEN o.id END ASC,
            CASE WHEN NOT $4 THEN o.id END DESC
        LIMIT $10",
        ticket_id,
        can_view_locked,
        cursor_at,
        after,
        i32::from(cursor_rank),
        cursor_id,
        stop_at,
        i32::from(stop_rank),
        stop_id,
        query_limit,
    )
    .fetch_all(db_pool)
    .await?
    .into_iter()
    .map(|x| {
        TicketThreadItem::Operation(TicketOperation {
            r#type: TicketThreadItemType::Operation,
            id: x.id,
            action: TicketOperationAction::from(x.action),
            actor: TicketAggreInfoUser {
                id: x.u_id,
                nickname: x.u_nickname,
                avatar: None,
                email: None,
            },
            actor_type: RbTicketSenderType::from_primitive(x.actor_type),
            message: make_message(
                x.m_id, x.mu_id, x.mu_n, x.m_st, x.m_ci, x.m_ca, x.m_ul, x.m_t, x.m_ct, x.m_c,
                x.m_u,
            ),
            ctime_at: x.ctime_at,
        })
    });

    items.extend(operations);
    items.sort_by(|a, b| {
        a.ctime_at()
            .cmp(&b.ctime_at())
            .then_with(|| a.same_time_rank().cmp(&b.same_time_rank()))
            .then_with(|| a.id().cmp(&b.id()))
    });

    let has_more = items.len() > limit;
    if after {
        items.truncate(limit);
    } else if items.len() > limit {
        items = items.split_off(items.len() - limit);
    }

    let before = (!after && has_more)
        .then(|| items.first().map(item_cursor).map(|cursor| cursor.encode()))
        .flatten();
    let next_after = (after && has_more)
        .then(|| items.last().map(item_cursor).map(|cursor| cursor.encode()))
        .flatten();

    let newer = items.last().map(item_cursor).map(|cursor| cursor.encode());
    Ok(TicketMessagePage {
        items,
        history: TicketHistory {
            before,
            after: next_after,
            stop: request.stop.as_ref().map(TicketCursor::encode),
            newer,
            has_more,
        },
    })
}

async fn get_ticket_page(
    db_pool: &DbPool,
    ticket_id: i32,
    can_view_locked: bool,
    is_puzzle: bool,
    request: &TicketPageRequest,
) -> Result<TicketMessagePage, RbInternalError> {
    if request.before.is_some() || request.after.is_some() {
        return get_ticket_messages_page(db_pool, ticket_id, can_view_locked, request, 50).await;
    }

    let mut latest = get_ticket_messages_page(
        db_pool,
        ticket_id,
        can_view_locked,
        &TicketPageRequest::default(),
        50,
    )
    .await?;
    if !is_puzzle || latest.history.before.is_none() {
        return Ok(latest);
    }

    let first = get_ticket_messages_page(
        db_pool,
        ticket_id,
        can_view_locked,
        &TicketPageRequest {
            oldest: true,
            ..Default::default()
        },
        1,
    )
    .await?;
    let Some(first_item) = first.items.into_iter().next() else {
        return Ok(latest);
    };
    let first_cursor = item_cursor(&first_item);
    let stop_cursor = latest.items.first().map(item_cursor);
    if !latest
        .items
        .iter()
        .any(|item| item_cursor(item).encode() == first_cursor.encode())
    {
        latest.items.insert(0, first_item);
    }
    let gap_probe = get_ticket_messages_page(
        db_pool,
        ticket_id,
        can_view_locked,
        &TicketPageRequest {
            after: Some(first_cursor.clone()),
            stop: stop_cursor.clone(),
            ..Default::default()
        },
        1,
    )
    .await?;
    if gap_probe.items.is_empty() {
        latest.history = TicketHistory {
            newer: latest
                .items
                .last()
                .map(item_cursor)
                .map(|cursor| cursor.encode()),
            ..Default::default()
        };
        return Ok(latest);
    }
    let newer = latest
        .items
        .last()
        .map(item_cursor)
        .map(|cursor| cursor.encode());
    latest.history = TicketHistory {
        after: Some(first_cursor.encode()),
        stop: stop_cursor.map(|cursor| cursor.encode()),
        newer,
        has_more: true,
        ..Default::default()
    };
    Ok(latest)
}

pub async fn get_ticket_message(
    db_pool: &DbPool,
    message_id: i32,
    can_view_locked: bool,
) -> Result<Option<TicketMessage>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT m.id, m.sender, m.sender_type, m.cost_id, m.cost_amount,
                m.unlocked, m.ctime_at, m.utime_at,
                u.id AS u_id, u.nickname AS u_nickname,
                CASE WHEN ($2 OR unlocked) THEN content ELSE NULL END AS content,
                CASE WHEN ($2 OR unlocked) THEN content_type ELSE NULL END AS content_type
        FROM rb_message m
        JOIN rb_user u ON u.id = m.sender
        WHERE m.id = $1",
        message_id,
        can_view_locked
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result.map(|x| TicketMessage {
        r#type: TicketThreadItemType::Message,
        id: x.id,
        sender: TicketAggreInfoUser {
            id: x.u_id,
            nickname: x.u_nickname,
            avatar: None,
            email: None,
        },
        sender_type: RbTicketSenderType::from_primitive(x.sender_type),
        cost_id: x.cost_id,
        cost_amount: x.cost_amount,
        unlocked: x.unlocked,

        content: x.content,
        content_type: x.content_type.map(RbContentType::from_primitive),
        ctime_at: x.ctime_at,
        utime_at: x.utime_at,
    }))
}

pub async fn get_or_create_dm_ticket_id(
    db_pool: &DbPool,
    team_id: i32,
    actor_id: i32,
    actor_type: RbTicketSenderType,
) -> Result<i32, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let result = sqlx::query!(
        "INSERT INTO rb_ticket (state, team_id, puzzle_id)
        VALUES (1, $1, NULL)
        ON CONFLICT (team_id) WHERE puzzle_id IS NULL
        DO UPDATE SET team_id = EXCLUDED.team_id
        RETURNING id, xmax = 0 AS inserted;",
        team_id
    )
    .fetch_one(&mut *tx)
    .await?;

    if result.inserted.unwrap_or(false) {
        sqlx::query!(
            "INSERT INTO rb_ticket_operation (ticket_id, action, actor, actor_type)
            VALUES ($1, $2, $3, $4)",
            result.id,
            i16::from(TicketOperationAction::Open),
            actor_id,
            i16::from(actor_type)
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(result.id)
}

pub async fn get_dm_ticket_id(
    db_pool: &DbPool,
    team_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM rb_ticket WHERE team_id = $1 AND puzzle_id IS NULL;",
        team_id
    )
    .fetch_optional(db_pool)
    .await?)
}

pub async fn get_dm_ticket_thread(
    db_pool: &DbPool,
    team_id: i32,
    can_view_locked: bool,
    can_use_trusted_content: bool,
    page: &TicketPageRequest,
) -> Result<TicketThread, RbInternalError> {
    let ticket_id = sqlx::query_scalar!(
        "SELECT id FROM rb_ticket
        WHERE team_id = $1 AND puzzle_id IS NULL;",
        team_id
    )
    .fetch_optional(db_pool)
    .await?;

    let feature_access = player_dm_feature_access(db_pool, team_id).await?;

    let Some(ticket_id) = ticket_id else {
        return Ok(TicketThread {
            ticket: None,
            messages: vec![],
            history: TicketHistory::default(),
            perm: TicketPerm::new(
                can_view_locked,
                can_view_locked,
                can_use_trusted_content,
                vec![],
                feature_access.send_block(false),
            ),
        });
    };

    let mut ticket = get_ticket_summary(db_pool, ticket_id, false).await?;
    if !can_view_locked && let Some(ticket) = ticket.as_mut() {
        ticket.hide_assignee();
    }
    let message_page = get_ticket_page(db_pool, ticket_id, can_view_locked, false, page).await?;
    let state = ticket
        .as_ref()
        .map(|x| x.state)
        .unwrap_or(RbTicketState::Invalid);
    let send_block = if feature_access.send_block(true) != TicketSendBlock::Ok {
        feature_access.send_block(true)
    } else {
        calc_send_block(db_pool, ticket_id, state, true, Some(3)).await?
    };

    Ok(TicketThread {
        ticket,
        messages: message_page.items,
        history: message_page.history,
        perm: TicketPerm::new(
            can_view_locked,
            can_view_locked,
            can_use_trusted_content,
            vec![],
            send_block,
        ),
    })
}

pub async fn get_ticket_summary(
    db_pool: &DbPool,
    ticket_id: i32,
    include_team: bool,
) -> Result<Option<TicketSummary>, RbInternalError> {
    let info = sqlx::query!(
        r#"WITH stats AS (
            SELECT m.ticket_id, COUNT(*) AS msg_count, MAX(m.ctime_at) AS last_at
            FROM rb_message m
            WHERE m.ticket_id = $1
            GROUP BY m.ticket_id
        ),
        last_msg AS (
            SELECT DISTINCT ON (m.ticket_id) m.ticket_id, m.sender_type
            FROM rb_message m
            WHERE m.ticket_id = $1
            ORDER BY m.ticket_id, m.ctime_at DESC, m.id DESC
        )
        SELECT tk.state,
                t.id AS t_id, t.name AS t_name,
                (CASE
                    WHEN t.is_banned THEN -1
                    WHEN t.finish_at IS NOT NULL THEN 2
                    WHEN t.is_locked THEN 1
                    ELSE 0
                END)::SMALLINT AS "t_state!",
                t.game_id AS g_id,
                p.id AS "p_id?", p.slug AS "p_slug?", p.title AS "p_title?", tp.state AS "p_state?",
                r.id AS "r_id?", r.slug AS "r_slug?", r.title AS "r_title?",
                stats.msg_count, stats.last_at,
                last_msg.sender_type AS "last_by?",
                au.id AS "a_id?", au.nickname AS "a_nickname?", au.email AS "a_email?",
                au.avatar_provider AS "a_avatar_provider?"
        FROM rb_ticket tk
        JOIN rb_team t ON t.id = tk.team_id
        LEFT JOIN rb_user au ON au.id = tk.assignee
        LEFT JOIN rb_puzzle p ON p.id = tk.puzzle_id
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.puzzle_id = p.id
        LEFT JOIN rb_round r ON r.id = p.round_id
        LEFT JOIN stats ON stats.ticket_id = tk.id
        LEFT JOIN last_msg ON last_msg.ticket_id = tk.id
        WHERE tk.id = $1"#,
        ticket_id
    )
    .fetch_optional(db_pool)
    .await?;

    let Some(x) = info else {
        return Ok(None);
    };
    let currency = if include_team {
        crate::db::team::get_currency_info(db_pool, x.t_id).await?
    } else {
        vec![]
    };

    Ok(Some(TicketSummary {
        id: ticket_id,
        state: RbTicketState::from_primitive(x.state),
        game_id: Some(x.g_id),
        team: include_team.then_some(TicketAggreInfoTeam {
            id: x.t_id,
            name: x.t_name,
            state: RbTeamState::from_primitive(x.t_state),
            currency,
        }),
        puzzle: make_puzzle(
            x.p_id, x.p_slug, x.p_title, x.p_state, x.r_id, x.r_slug, x.r_title,
        ),
        msg_count: x.msg_count,
        last_at: x.last_at,
        last_by: x.last_by.map(RbTicketSenderType::from_primitive),
        assignee: x
            .a_id
            .zip(x.a_nickname)
            .map(|(id, nickname)| TicketAggreInfoUser {
                id,
                nickname,
                avatar: user_avatar(x.a_email.as_deref(), x.a_avatar_provider),
                email: x.a_email,
            }),
    }))
}

pub async fn get_ticket_thread(
    db_pool: &DbPool,
    ticket_id: i32,
    info: &TicketUserInfo,
    page: &TicketPageRequest,
) -> Result<Option<TicketThread>, RbInternalError> {
    let Some(mut ticket) = get_ticket_summary(db_pool, ticket_id, true).await? else {
        return Ok(None);
    };
    if !info.mod_access {
        ticket.hide_assignee();
    }
    let message_page = get_ticket_page(
        db_pool,
        ticket_id,
        info.mod_access,
        ticket.is_puzzle_ticket(),
        page,
    )
    .await?;
    let feature_access = player_ticket_feature_access(db_pool, ticket_id).await?;
    let feature_block = feature_access.send_block(true);
    let send_block = if info.member_access && feature_block != TicketSendBlock::Ok {
        feature_block
    } else {
        calc_send_block(
            db_pool,
            ticket_id,
            ticket.state,
            info.member_access || info.mod_access,
            Some(1),
        )
        .await?
    };
    let currency = if info.mod_access {
        ticket.currency_ids()
    } else {
        vec![]
    };

    Ok(Some(TicketThread {
        ticket: Some(ticket),
        messages: message_page.items,
        history: message_page.history,
        perm: TicketPerm::new(
            info.mod_access,
            info.mod_access,
            info.admin_access,
            currency,
            send_block,
        ),
    }))
}

fn make_puzzle(
    id: Option<i32>,
    slug: Option<String>,
    title: Option<String>,
    state: Option<i16>,
    round_id: Option<i32>,
    round_slug: Option<String>,
    round_title: Option<String>,
) -> Option<TicketAggreInfoPuzzle> {
    match (id, title, state, round_id, round_title) {
        (Some(id), Some(title), Some(state), Some(round_id), Some(round_title)) => {
            Some(TicketAggreInfoPuzzle {
                id,
                slug,
                title,
                state: RbTeamPuzzleState::from_primitive(state),
                round: RbRoundSimpleData {
                    id: round_id,
                    slug: round_slug,
                    title: round_title,
                },
            })
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn list_staff_tickets(
    db_pool: &DbPool,
    game_id: i32,
    kind: i32,
    state: Option<i16>,
    waiting_for: i32,
    assignee_filter: i32,
    user_id: i32,
    puzzle_id: Option<i32>,
    team_id: Option<i32>,
    limit: i64,
    offset: i64,
) -> Result<Vec<TicketSummary>, RbInternalError> {
    let rows = sqlx::query!(
        r#"SELECT tk.id, tk.state,
                t.id AS t_id, t.name AS t_name,
                (CASE
                    WHEN t.is_banned THEN -1
                    WHEN t.finish_at IS NOT NULL THEN 2
                    WHEN t.is_locked THEN 1
                    ELSE 0
                END)::SMALLINT AS "t_state!",
                t.game_id AS g_id,
                p.id AS "p_id?", p.slug AS "p_slug?", p.title AS "p_title?",
                COALESCE(tp.state, -1)::SMALLINT AS "p_state?",
                r.id AS "r_id?", r.slug AS "r_slug?", r.title AS "r_title?",
                au.id AS "a_id?", au.nickname AS "a_nickname?", au.email AS "a_email?",
                au.avatar_provider AS "a_avatar_provider?",
                (SELECT COUNT(*) FROM rb_message mc WHERE mc.ticket_id = tk.id) AS msg_count,
                lm.ctime_at AS "last_at?", lm.sender_type AS "last_by?"
        FROM rb_ticket tk
        JOIN rb_team t ON t.id = tk.team_id
        LEFT JOIN rb_puzzle p ON p.id = tk.puzzle_id
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.puzzle_id = p.id
        LEFT JOIN rb_round r ON r.id = p.round_id
        LEFT JOIN rb_user au ON au.id = tk.assignee
        LEFT JOIN LATERAL (
            SELECT m.sender_type, m.ctime_at
            FROM rb_message m
            WHERE m.ticket_id = tk.id
            ORDER BY m.id DESC
            LIMIT 1
        ) lm ON TRUE
        WHERE t.game_id = $1
            AND ($2 = 0 OR ($2 = 1 AND tk.puzzle_id IS NOT NULL) OR ($2 = 2 AND tk.puzzle_id IS NULL))
            AND ($3::SMALLINT IS NULL OR tk.state = $3)
            AND ($4 = 0 OR ($4 = 1 AND lm.sender_type = 0) OR ($4 = 2 AND lm.sender_type = 1))
            AND ($5 = 0 OR ($5 = 1 AND tk.assignee = $6) OR ($5 = 2 AND tk.assignee IS NULL))
            AND ($7::INT IS NULL OR tk.puzzle_id = $7)
            AND ($8::INT IS NULL OR tk.team_id = $8)
        ORDER BY lm.ctime_at DESC NULLS LAST, tk.id DESC
        LIMIT $9 OFFSET $10"#,
        game_id,
        kind,
        state,
        waiting_for,
        assignee_filter,
        user_id,
        puzzle_id,
        team_id,
        limit,
        offset,
    )
    .fetch_all(db_pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|x| TicketSummary {
            id: x.id,
            state: RbTicketState::from_primitive(x.state),
            game_id: Some(x.g_id),
            team: Some(TicketAggreInfoTeam {
                id: x.t_id,
                name: x.t_name,
                state: RbTeamState::from_primitive(x.t_state),
                currency: vec![],
            }),
            puzzle: make_puzzle(
                x.p_id, x.p_slug, x.p_title, x.p_state, x.r_id, x.r_slug, x.r_title,
            ),
            msg_count: x.msg_count,
            last_at: x.last_at,
            last_by: x.last_by.map(RbTicketSenderType::from_primitive),
            assignee: x
                .a_id
                .zip(x.a_nickname)
                .map(|(id, nickname)| TicketAggreInfoUser {
                    id,
                    nickname,
                    avatar: user_avatar(x.a_email.as_deref(), x.a_avatar_provider),
                    email: x.a_email,
                }),
        })
        .collect())
}

pub enum AssignTicketResult {
    Ok(TicketAggreInfoUser),
    Assigned(TicketAggreInfoUser),
    NotFound,
}

pub async fn assign_ticket_self(
    db_pool: &DbPool,
    ticket_id: i32,
    user_id: i32,
    force: bool,
) -> Result<AssignTicketResult, RbInternalError> {
    let mut tx = db_pool.begin().await?;
    let current = sqlx::query!(
        "SELECT tk.assignee, u.nickname AS \"nickname?\", u.email AS \"email?\",
            u.avatar_provider AS \"avatar_provider?\"
        FROM rb_ticket tk
        LEFT JOIN rb_user u ON u.id = tk.assignee
        WHERE tk.id = $1
        FOR UPDATE OF tk",
        ticket_id,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(current) = current else {
        return Ok(AssignTicketResult::NotFound);
    };
    if let Some(assignee) = current.assignee
        && assignee != user_id
        && !force
    {
        return Ok(AssignTicketResult::Assigned(TicketAggreInfoUser {
            id: assignee,
            nickname: current.nickname.unwrap_or_default(),
            avatar: user_avatar(current.email.as_deref(), current.avatar_provider),
            email: current.email,
        }));
    }

    let user = sqlx::query!(
        "UPDATE rb_ticket SET assignee = $2 WHERE id = $1
        RETURNING
            (SELECT nickname FROM rb_user WHERE id = $2) AS \"nickname!\",
            (SELECT email FROM rb_user WHERE id = $2) AS \"email!\",
            (SELECT avatar_provider FROM rb_user WHERE id = $2) AS \"avatar_provider!\"",
        ticket_id,
        user_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(AssignTicketResult::Ok(TicketAggreInfoUser {
        id: user_id,
        nickname: user.nickname,
        avatar: user_avatar(Some(&user.email), Some(user.avatar_provider)),
        email: Some(user.email),
    }))
}

pub async fn unassign_ticket(
    db_pool: &DbPool,
    ticket_id: i32,
    user_id: i32,
) -> Result<bool, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "UPDATE rb_ticket SET assignee = NULL
        WHERE id = $1 AND assignee = $2
        RETURNING id",
        ticket_id,
        user_id,
    )
    .fetch_optional(db_pool)
    .await?
    .is_some())
}

#[derive(Deserialize)]
pub struct SendMessageData {
    pub content: String,
    pub content_type: RbContentType,
    pub sender_id: i32,
    pub sender_type: RbTicketSenderType,
    pub cost_id: Option<i32>,
    pub cost_amount: i64,
}

async fn insert_ticket_message_conn(
    conn: &mut PgConnection,
    ticket_id: i32,
    data: &SendMessageData,
) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_message
            (content, content_type, sender, sender_type,
            cost_id, cost_amount, unlocked, ticket_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id;",
        data.content,
        i16::from(data.content_type),
        data.sender_id,
        i16::from(data.sender_type),
        data.cost_id,
        data.cost_amount,
        data.cost_id.is_none(),
        ticket_id
    )
    .fetch_one(&mut *conn)
    .await?;

    if matches!(data.sender_type, RbTicketSenderType::Host) {
        sqlx::query!(
            "INSERT INTO rb_notification (team_id, kind, source_id, actor, data)
            SELECT tk.team_id, $2::SMALLINT, $1::INT, $3::INT,
                jsonb_build_object(
                    'ticket_id', tk.id,
                    'message_id', $1,
                    'puzzle_id', p.id,
                    'puzzle_title', p.title
                )
            FROM rb_ticket tk
            LEFT JOIN rb_puzzle p ON p.id = tk.puzzle_id
            WHERE tk.id = $4",
            result,
            i16::from(crate::db::notification::NotificationKind::TicketReply),
            data.sender_id,
            ticket_id,
        )
        .execute(&mut *conn)
        .await?;
    }

    Ok(result)
}

async fn auto_assign_ticket_on_staff_message_conn(
    conn: &mut PgConnection,
    ticket_id: i32,
    data: &SendMessageData,
) -> Result<(), RbInternalError> {
    if matches!(data.sender_type, RbTicketSenderType::Host) {
        sqlx::query!(
            "UPDATE rb_ticket
            SET assignee = $2
            WHERE id = $1 AND assignee IS NULL",
            ticket_id,
            data.sender_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub enum SendTicketMessageResult {
    Ok(TicketMessage),
    Pending,
    Assigned(TicketAggreInfoUser),
}

pub async fn send_ticket_message(
    db_pool: &DbPool,
    ticket_id: i32,
    data: &SendMessageData,
    max_pending: Option<i64>,
    force_assignee: bool,
) -> Result<SendTicketMessageResult, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let ticket = sqlx::query!(
        "SELECT tk.assignee, u.nickname AS \"nickname?\", u.email AS \"email?\",
            u.avatar_provider AS \"avatar_provider?\"
        FROM rb_ticket tk
        LEFT JOIN rb_user u ON u.id = tk.assignee
        WHERE tk.id = $1
        FOR UPDATE OF tk",
        ticket_id,
    )
    .fetch_one(&mut *tx)
    .await?;

    if matches!(data.sender_type, RbTicketSenderType::Host)
        && let Some(assignee) = ticket.assignee
        && assignee != data.sender_id
        && !force_assignee
    {
        return Ok(SendTicketMessageResult::Assigned(TicketAggreInfoUser {
            id: assignee,
            nickname: ticket.nickname.unwrap_or_default(),
            avatar: user_avatar(ticket.email.as_deref(), ticket.avatar_provider),
            email: ticket.email,
        }));
    }

    if let Some(max_pending) = max_pending
        && matches!(data.sender_type, RbTicketSenderType::Team)
    {
        // check pending message
        let pending = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM rb_message m
            WHERE ticket_id = $1
            AND sender_type = 0
            AND NOT EXISTS (
                SELECT 1
                FROM rb_message AS reply
                WHERE reply.ticket_id = m.ticket_id
                    AND reply.sender_type = 1
                    AND reply.id > m.id
            );",
            ticket_id
        )
        .fetch_one(&mut *tx)
        .await?
        .unwrap_or(0);

        if pending >= max_pending {
            return Ok(SendTicketMessageResult::Pending);
        }
    }

    auto_assign_ticket_on_staff_message_conn(&mut tx, ticket_id, data).await?;

    let result = insert_ticket_message_conn(&mut tx, ticket_id, data).await?;

    tx.commit().await?;

    let message = get_ticket_message(db_pool, result, true)
        .await?
        .ok_or("Inserted ticket message not found")?;
    Ok(SendTicketMessageResult::Ok(message))
}

pub enum CloseTicketResult {
    Ok(Option<i32>),
    Closed,
    Assigned(TicketAggreInfoUser),
}

pub async fn close_ticket(
    db_pool: &DbPool,
    ticket_id: i32,
    actor_id: i32,
    actor_type: RbTicketSenderType,
    message: Option<&SendMessageData>,
    force_assignee: bool,
) -> Result<CloseTicketResult, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    let info = sqlx::query!(
        r#"SELECT t.team_id, t.puzzle_id, t.state, t.assignee,
            au.nickname AS "assignee_nickname?", au.email AS "assignee_email?",
            au.avatar_provider AS "assignee_avatar_provider?", tm.game_id,
            p.round_id AS "round_id?", p.title AS "puzzle_title?"
        FROM rb_ticket t
        JOIN rb_team tm ON tm.id = t.team_id
        LEFT JOIN rb_puzzle p ON p.id = t.puzzle_id
        LEFT JOIN rb_user au ON au.id = t.assignee
        WHERE t.id = $1
        FOR UPDATE OF t;"#,
        ticket_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(info) = info else {
        return Ok(CloseTicketResult::Closed);
    };

    if info.state == i16::from(RbTicketState::Closed) {
        return Ok(CloseTicketResult::Closed);
    }
    if message.is_some()
        && matches!(actor_type, RbTicketSenderType::Host)
        && let Some(assignee) = info.assignee
        && assignee != actor_id
        && !force_assignee
    {
        return Ok(CloseTicketResult::Assigned(TicketAggreInfoUser {
            id: assignee,
            nickname: info.assignee_nickname.unwrap_or_default(),
            avatar: user_avatar(
                info.assignee_email.as_deref(),
                info.assignee_avatar_provider,
            ),
            email: info.assignee_email,
        }));
    }

    let updated = sqlx::query_scalar!(
        "UPDATE rb_ticket SET state = $1
        WHERE id = $2 AND state <> $1
        RETURNING id;",
        i16::from(RbTicketState::Closed),
        ticket_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();

    let mut inserted_message_id = None;
    if updated {
        let message_id = if let Some(message) = message {
            auto_assign_ticket_on_staff_message_conn(&mut tx, ticket_id, message).await?;
            Some(insert_ticket_message_conn(&mut tx, ticket_id, message).await?)
        } else {
            None
        };
        inserted_message_id = message_id;

        sqlx::query!(
            "INSERT INTO rb_ticket_operation (ticket_id, action, actor, actor_type, message_id)
            VALUES ($1, $2, $3, $4, $5)",
            ticket_id,
            i16::from(TicketOperationAction::Close),
            actor_id,
            i16::from(actor_type),
            message_id
        )
        .execute(&mut *tx)
        .await?;

        crate::db::event_log::insert_conn(
            &mut tx,
            crate::db::event_log::EventLogInput {
                event_type: "ticket.closed",
                event_scope: i16::from(crate::db::event_log::EventScope::TeamActivity),
                severity: i16::from(crate::db::event_log::EventSeverity::Info),
                game_id: Some(info.game_id),
                team_id: Some(info.team_id),
                user_id: Some(actor_id),
                puzzle_id: info.puzzle_id,
                round_id: info.round_id,
                ticket_id: Some(ticket_id),
                data: json!({
                    "staff": matches!(actor_type, RbTicketSenderType::Host),
                    "ticket": { "id": ticket_id },
                    "puzzle": {
                        "id": info.puzzle_id,
                        "title": info.puzzle_title
                    }
                }),
                ..Default::default()
            },
        )
        .await?;
    }

    tx.commit().await?;

    Ok(if updated {
        CloseTicketResult::Ok(inserted_message_id)
    } else {
        CloseTicketResult::Closed
    })
}

pub async fn close_puzzle_tickets_on_solve_conn(
    conn: &mut PgConnection,
    team_id: i32,
    puzzle_id: i32,
    actor_id: i32,
) -> Result<(), RbInternalError> {
    let ticket_ids = sqlx::query_scalar!(
        "UPDATE rb_ticket
        SET state = $1
        WHERE team_id = $2 AND puzzle_id = $3 AND state <> $1
        RETURNING id;",
        i16::from(RbTicketState::Closed),
        team_id,
        puzzle_id
    )
    .fetch_all(&mut *conn)
    .await?;

    for ticket_id in ticket_ids {
        sqlx::query!(
            "INSERT INTO rb_ticket_operation (ticket_id, action, actor, actor_type)
            VALUES ($1, $2, $3, $4)",
            ticket_id,
            i16::from(TicketOperationAction::AutoCloseSolved),
            actor_id,
            i16::from(RbTicketSenderType::Team)
        )
        .execute(&mut *conn)
        .await?;
    }

    Ok(())
}

pub enum PurchaseTicketMessageResult {
    Insufficient,
    Unavailable,
    Ok(TicketMessage),
}

pub async fn purchase_ticket_message(
    app: &AppState,
    needs_pay: bool,
    user_id: i32,
    ticket_id: i32,
    message_id: i32,
) -> Result<PurchaseTicketMessageResult, RbInternalError> {
    let mut tx = app.db.begin().await?;

    let info = sqlx::query!(
        r#"SELECT t.team_id, t.puzzle_id, tm.game_id,
            p.round_id AS "round_id?", p.title AS "puzzle_title?",
            m.cost_id, m.cost_amount
        FROM rb_message m
        JOIN rb_ticket t ON t.id = m.ticket_id
        JOIN rb_team tm ON tm.id = t.team_id
        LEFT JOIN rb_puzzle p ON p.id = t.puzzle_id
        WHERE m.id = $1 AND t.id = $2
            AND NOT m.unlocked
        FOR UPDATE OF m;"#,
        message_id,
        ticket_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(info) = info else {
        return Ok(PurchaseTicketMessageResult::Unavailable);
    };

    let mut currency_event: Option<crate::db::event_log::CurrencyEventData> = None;

    if info.cost_id.is_some() && needs_pay {
        let result = sqlx::query!(
            r#"WITH current AS (
                SELECT tc.team_id, c.id, c.slug, c.cname, c.prec,
                    LEAST(
                        tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                        c.max_amount::NUMERIC
                    )::BIGINT AS current_amount
                FROM rb_team_currency tc
                JOIN rb_currency c ON tc.currency_id = c.id
                WHERE tc.team_id = $1 AND c.id = $2
                    AND c.game_id = (
                        SELECT tm.game_id
                        FROM rb_team_member tm
                        WHERE tm.team_id = $1 AND tm.user_id = $4
                    )
                    AND ($3::BIGINT <= 0 OR LEAST(
                        tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                        c.max_amount::NUMERIC
                    )::BIGINT >= $3)
                FOR UPDATE
            ), updated AS (
                UPDATE rb_team_currency tc
                SET utime_at = NOW(), amount = current.current_amount - $3
                FROM current
                WHERE tc.team_id = current.team_id AND tc.currency_id = current.id
                RETURNING current.id, current.slug, current.cname, current.prec,
                    current.current_amount, tc.amount
            )
            SELECT id AS "id!", slug AS "slug!", cname AS "name!", prec AS "prec!",
                current_amount AS "before!", amount AS "after!"
            FROM updated;"#,
            info.team_id,
            info.cost_id,
            info.cost_amount,
            user_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(currency) = result {
            currency_event = Some(crate::db::event_log::CurrencyEventData {
                id: currency.id,
                slug: currency.slug,
                name: currency.name,
                prec: currency.prec,
                before: currency.before,
                after: currency.after,
            });
        } else {
            return Ok(PurchaseTicketMessageResult::Insufficient);
        }
    }

    let updated = sqlx::query_scalar!(
        "UPDATE rb_message
        SET unlocked = TRUE, utime_at = NOW()
        WHERE id = $1 AND ticket_id = $2 AND NOT unlocked
        RETURNING id;",
        message_id,
        ticket_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    if updated.is_none() {
        return Ok(PurchaseTicketMessageResult::Unavailable);
    }

    crate::db::event_log::insert_conn(
        &mut tx,
        crate::db::event_log::EventLogInput {
            event_type: "ticket.message_purchased",
            event_scope: i16::from(crate::db::event_log::EventScope::TeamActivity),
            severity: i16::from(crate::db::event_log::EventSeverity::Info),
            game_id: Some(info.game_id),
            team_id: Some(info.team_id),
            user_id: Some(user_id),
            puzzle_id: info.puzzle_id,
            round_id: info.round_id,
            ticket_id: Some(ticket_id),
            currency_id: currency_event.as_ref().map(|currency| currency.id),
            delta_amount: currency_event.as_ref().map(|currency| currency.delta()),
            data: json!({
                "staff": !needs_pay,
                "ticket": { "id": ticket_id },
                "message": { "id": message_id },
                "puzzle": {
                    "id": info.puzzle_id,
                    "title": info.puzzle_title
                },
                "cost": {
                    "currency_id": info.cost_id,
                    "amount": info.cost_amount
                },
                "currency": currency_event.as_ref().map(|currency| json!({
                    "id": currency.id,
                    "slug": currency.slug,
                    "name": currency.name,
                    "prec": currency.prec
                })),
                "delta": currency_event.as_ref().map(|currency| currency.delta()),
                "before": currency_event.as_ref().map(|currency| currency.before),
                "after": currency_event.as_ref().map(|currency| currency.after)
            }),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;

    let message = get_ticket_message(&app.db, message_id, true)
        .await?
        .ok_or("Unlocked ticket message not found")?;

    Ok(PurchaseTicketMessageResult::Ok(message))
}

pub enum OpenPuzzleTicketResult {
    Ok(Box<TicketThread>),
    PendingExists,
    Cooldown,
    Disabled,
    FeatureClosed,
    FeatureExistingOnly,
    TeamFeatureBanned,
}

pub async fn open_puzzle_ticket(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
    message: &SendMessageData,
) -> Result<OpenPuzzleTicketResult, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    sqlx::query!("SELECT FROM rb_team WHERE id = $1 FOR UPDATE", team_id)
        .execute(&mut *tx)
        .await?;

    // check pending ticket
    let pending = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_ticket
            WHERE state = 1 AND team_id = $1 AND puzzle_id IS NOT NULL
        );",
        team_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(false);
    if pending {
        return Ok(OpenPuzzleTicketResult::PendingExists);
    }

    // check puzzle ticket availability
    let ticket_availability = sqlx::query!(
        "SELECT p.ticket_enabled, p.title AS puzzle_title, p.round_id, r.game_id,
            EXISTS (
                SELECT 1 FROM rb_team_puzzle tp
                JOIN rb_puzzle sp ON sp.id = tp.puzzle_id
                JOIN rb_puzzle_effective_release srp ON srp.puzzle_id = sp.id
                WHERE tp.puzzle_id = p.id AND tp.team_id = $1
                    AND tp.state >= 0
                    AND GREATEST(tp.ctime_at, srp.release_at) <= NOW() - (p.ticket_cooldown * INTERVAL '1 second')
            ) AS cooldown_ready
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        WHERE p.id = $2
            AND rp.release_at <= NOW();",
        team_id,
        puzzle_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(ticket_availability) = ticket_availability else {
        return Ok(OpenPuzzleTicketResult::Disabled);
    };
    if matches!(message.sender_type, RbTicketSenderType::Team) {
        let access = sqlx::query_scalar!(
            "SELECT (CASE
                WHEN NOT COALESCE(tf.enabled, TRUE) THEN -2
                ELSE COALESCE(gf.state, 2)
            END)::SMALLINT AS \"access!\"
            FROM rb_team t
            LEFT JOIN rb_game_feature gf ON gf.game_id = t.game_id
                AND gf.feature_type = 2
            LEFT JOIN rb_team_feature tf ON tf.team_id = t.id
                AND tf.feature_type = 2
            WHERE t.id = $1 AND t.game_id = $2;",
            team_id,
            ticket_availability.game_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(0);
        match PlayerFeatureAccess::from_value(access) {
            PlayerFeatureAccess::Open => {}
            PlayerFeatureAccess::ExistingOnly => {
                return Ok(OpenPuzzleTicketResult::FeatureExistingOnly);
            }
            PlayerFeatureAccess::GameClosed => {
                return Ok(OpenPuzzleTicketResult::FeatureClosed);
            }
            PlayerFeatureAccess::TeamFeatureBanned => {
                return Ok(OpenPuzzleTicketResult::TeamFeatureBanned);
            }
        }
    }
    if !ticket_availability.ticket_enabled {
        return Ok(OpenPuzzleTicketResult::Disabled);
    }
    if !ticket_availability.cooldown_ready.unwrap_or(false) {
        return Ok(OpenPuzzleTicketResult::Cooldown);
    }

    let ticket_id = sqlx::query_scalar!(
        "INSERT INTO rb_ticket (state, team_id, puzzle_id)
        VALUES (1, $1, $2)
        RETURNING id;",
        team_id,
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?;

    let message_id = insert_ticket_message_conn(&mut tx, ticket_id, message).await?;

    sqlx::query!(
        "INSERT INTO rb_ticket_operation (ticket_id, action, actor, actor_type, message_id)
        VALUES ($1, $2, $3, $4, $5)",
        ticket_id,
        i16::from(TicketOperationAction::Open),
        message.sender_id,
        i16::from(message.sender_type),
        message_id
    )
    .execute(&mut *tx)
    .await?;

    crate::db::event_log::insert_conn(
        &mut tx,
        crate::db::event_log::EventLogInput {
            event_type: "ticket.opened",
            event_scope: i16::from(crate::db::event_log::EventScope::TeamActivity),
            severity: i16::from(crate::db::event_log::EventSeverity::Info),
            game_id: Some(ticket_availability.game_id),
            team_id: Some(team_id),
            user_id: Some(message.sender_id),
            puzzle_id: Some(puzzle_id),
            round_id: Some(ticket_availability.round_id),
            ticket_id: Some(ticket_id),
            data: json!({
                "ticket": { "id": ticket_id },
                "puzzle": {
                    "id": puzzle_id,
                    "title": ticket_availability.puzzle_title
                }
            }),
            ..Default::default()
        },
    )
    .await?;

    tx.commit().await?;

    let info = TicketUserInfo {
        ticket_id,
        state: RbTicketState::Open,
        member_access: matches!(message.sender_type, RbTicketSenderType::Team),
        mod_access: matches!(message.sender_type, RbTicketSenderType::Host),
        admin_access: false,
    };
    let thread = get_ticket_thread(db_pool, ticket_id, &info, &TicketPageRequest::default())
        .await?
        .ok_or("Opened ticket not found")?;

    Ok(OpenPuzzleTicketResult::Ok(Box::new(thread)))
}

pub async fn get_team_puzzle_tickets(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
    feature_exempt: bool,
) -> Result<TicketPuzzleList, RbInternalError> {
    let info = sqlx::query!(
        "WITH stats AS (
            SELECT m.ticket_id, COUNT(*) AS msg_count, MAX(m.ctime_at) AS last_at
            FROM rb_message m
            GROUP BY m.ticket_id
        ),
        last_msg AS (
            SELECT DISTINCT ON (m.ticket_id) m.ticket_id, m.sender_type
            FROM rb_message m
            ORDER BY m.ticket_id, m.ctime_at DESC, m.id DESC
        )
        SELECT tk.id, tk.state, stats.msg_count, stats.last_at,
            last_msg.sender_type AS \"last_by?\"
        FROM rb_ticket tk
        LEFT JOIN stats ON stats.ticket_id = tk.id
        LEFT JOIN last_msg ON last_msg.ticket_id = tk.id
        WHERE tk.team_id = $1 AND tk.puzzle_id = $2
        ORDER BY tk.id DESC",
        team_id,
        puzzle_id
    )
    .fetch_all(db_pool)
    .await?;

    let open_tickets = sqlx::query_as!(
        TicketOpenInfo,
        "SELECT tk.id, p.id AS puzzle_id, p.title AS puzzle_title
        FROM rb_ticket tk
        JOIN rb_puzzle p ON p.id = tk.puzzle_id
        WHERE tk.state = $1 AND tk.team_id = $2 AND tk.puzzle_id IS NOT NULL
        ORDER BY tk.id DESC;",
        i16::from(RbTicketState::Open),
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    let has_current_puzzle_open = open_tickets
        .iter()
        .any(|ticket| ticket.puzzle_id == puzzle_id);
    let pending_limit_reached = !open_tickets.is_empty();

    let cooldown = sqlx::query!(
        "SELECT
            (SELECT p.ticket_enabled
                FROM rb_puzzle p
                JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
                WHERE p.id = $2 AND tp.state >= 0
                    AND rp.release_at <= NOW()
            ) AS ticket_enabled,
            EXISTS (
                SELECT 1 FROM rb_puzzle p
                JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
                WHERE p.id = $2 AND p.ticket_enabled
                    AND tp.state >= 0
                    AND rp.release_at <= NOW()
                    AND GREATEST(tp.ctime_at, rp.release_at) <= NOW() - (p.ticket_cooldown * INTERVAL '1 second')
            ) AS ready,
            (
                SELECT GREATEST(tp.ctime_at, rp.release_at) + (p.ticket_cooldown * INTERVAL '1 second')
                FROM rb_puzzle p
                JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
                WHERE p.id = $2 AND p.ticket_enabled
                    AND tp.state >= 0
                    AND rp.release_at <= NOW()
            ) AS cooldown_till;",
        team_id,
        puzzle_id
    )
    .fetch_one(db_pool)
    .await?;

    let feature_access = sqlx::query_scalar!(
        "SELECT (CASE
            WHEN NOT COALESCE(tf.enabled, TRUE) THEN -2
            ELSE COALESCE(gf.state, 2)
        END)::SMALLINT AS \"access!\"
        FROM rb_puzzle p
        JOIN rb_round r ON r.id = p.round_id
        JOIN rb_team t ON t.id = $2 AND t.game_id = r.game_id
        LEFT JOIN rb_game_feature gf ON gf.game_id = r.game_id AND gf.feature_type = 2
        LEFT JOIN rb_team_feature tf ON tf.team_id = t.id AND tf.feature_type = 2
        WHERE p.id = $1;",
        puzzle_id,
        team_id
    )
    .fetch_optional(db_pool)
    .await?
    .map(PlayerFeatureAccess::from_value)
    .unwrap_or(PlayerFeatureAccess::GameClosed);

    let open_block = if !feature_exempt && feature_access == PlayerFeatureAccess::TeamFeatureBanned
    {
        TicketOpenBlock::TeamFeatureBanned
    } else if !feature_exempt && feature_access == PlayerFeatureAccess::GameClosed {
        TicketOpenBlock::FeatureClosed
    } else if !feature_exempt && feature_access == PlayerFeatureAccess::ExistingOnly {
        TicketOpenBlock::FeatureExistingOnly
    } else if has_current_puzzle_open {
        TicketOpenBlock::CurrentPuzzlePending
    } else if !cooldown.ticket_enabled.unwrap_or(false) {
        TicketOpenBlock::Disabled
    } else if !cooldown.ready.unwrap_or(false) {
        TicketOpenBlock::Cooldown
    } else if pending_limit_reached {
        TicketOpenBlock::PendingLimit
    } else {
        TicketOpenBlock::Ok
    };

    let tickets = info
        .into_iter()
        .map(|x| TicketSummary {
            id: x.id,
            state: RbTicketState::from_primitive(x.state),
            game_id: None,
            team: None,
            puzzle: None,
            msg_count: x.msg_count,
            last_at: x.last_at,
            last_by: x.last_by.map(RbTicketSenderType::from_primitive),
            assignee: None,
        })
        .collect();

    Ok(TicketPuzzleList {
        open_block,
        cooldown_till: if matches!(open_block, TicketOpenBlock::Cooldown) {
            cooldown.cooldown_till
        } else {
            None
        },
        open_tickets: if matches!(open_block, TicketOpenBlock::PendingLimit) {
            open_tickets
        } else {
            vec![]
        },
        tickets,
    })
}

#[cfg(test)]
mod tests {
    use super::{PlayerFeatureAccess, TicketSendBlock};

    #[test]
    fn feature_access_distinguishes_new_and_existing_conversations() {
        assert_eq!(
            PlayerFeatureAccess::Open.send_block(false),
            TicketSendBlock::Ok
        );
        assert_eq!(
            PlayerFeatureAccess::ExistingOnly.send_block(false),
            TicketSendBlock::FeatureExistingOnly
        );
        assert_eq!(
            PlayerFeatureAccess::ExistingOnly.send_block(true),
            TicketSendBlock::Ok
        );
        assert_eq!(
            PlayerFeatureAccess::GameClosed.send_block(true),
            TicketSendBlock::FeatureClosed
        );
        assert_eq!(
            PlayerFeatureAccess::TeamFeatureBanned.send_block(true),
            TicketSendBlock::TeamFeatureBanned
        );
    }
}

use num_enum::{FromPrimitive, IntoPrimitive};
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
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
    pub puzzle_id: Option<i32>,
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
        "SELECT t.state, t.puzzle_id, u.urole,
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
        puzzle_id: x.puzzle_id,
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

#[derive(Serialize)]
pub struct TicketAggreInfoUser {
    id: i32,
    nickname: String,
}

#[derive(Serialize)]
pub struct TicketMessage {
    r#type: TicketThreadItemType,
    pub id: i32,
    sender: TicketAggreInfoUser,
    sender_type: RbTicketSenderType,
    cost_id: Option<i32>,
    cost_amount: i32,
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
}

impl TicketSummary {
    pub fn id(&self) -> i32 {
        self.id
    }

    pub fn currency_ids(&self) -> Vec<i32> {
        self.team
            .as_ref()
            .map(|team| team.currency.iter().map(|currency| currency.id).collect())
            .unwrap_or_default()
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
#[derive(Clone, Serialize_repr)]
pub enum TicketSendBlock {
    Ok = 0,
    NoAccess = -1,
    Closed = -2,
    Pending = -3,
}

#[derive(Serialize)]
pub struct TicketThread {
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket: Option<TicketSummary>,
    messages: Vec<TicketThreadItem>,
    perm: TicketPerm,
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
    cost_amount: Option<i32>,
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

pub async fn get_ticket_messages(
    db_pool: &DbPool,
    ticket_id: i32,
    can_view_locked: bool,
) -> Result<Vec<TicketThreadItem>, RbInternalError> {
    let mut items: Vec<TicketThreadItem> = Vec::new();

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
        ORDER BY m.ctime_at ASC",
        ticket_id,
        can_view_locked
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
        WHERE o.ticket_id = $1",
        ticket_id,
        can_view_locked
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

    Ok(items)
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
            i16::from(RbTicketSenderType::Team)
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(result.id)
}

pub async fn get_dm_ticket_thread(
    db_pool: &DbPool,
    team_id: i32,
    can_view_locked: bool,
    can_use_trusted_content: bool,
) -> Result<TicketThread, RbInternalError> {
    let ticket_id = sqlx::query_scalar!(
        "SELECT id FROM rb_ticket
        WHERE team_id = $1 AND puzzle_id IS NULL;",
        team_id
    )
    .fetch_optional(db_pool)
    .await?;

    let Some(ticket_id) = ticket_id else {
        return Ok(TicketThread {
            ticket: None,
            messages: vec![],
            perm: TicketPerm::new(
                can_view_locked,
                can_view_locked,
                can_use_trusted_content,
                vec![],
                TicketSendBlock::Ok,
            ),
        });
    };

    let ticket = get_ticket_summary(db_pool, ticket_id, false).await?;
    let messages = get_ticket_messages(db_pool, ticket_id, can_view_locked).await?;
    let state = ticket
        .as_ref()
        .map(|x| x.state)
        .unwrap_or(RbTicketState::Invalid);
    let send_block = calc_send_block(db_pool, ticket_id, state, true, Some(3)).await?;

    Ok(TicketThread {
        ticket,
        messages,
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
        "WITH stats AS (
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
                t.id AS t_id, t.name AS t_name, t.state AS t_state, t.game_id AS g_id,
                p.id AS \"p_id?\", p.slug AS \"p_slug?\", p.title AS \"p_title?\", tp.state AS \"p_state?\",
                r.id AS \"r_id?\", r.slug AS \"r_slug?\", r.title AS \"r_title?\",
                stats.msg_count, stats.last_at,
                last_msg.sender_type AS \"last_by?\"
        FROM rb_ticket tk
        JOIN rb_team t ON t.id = tk.team_id
        LEFT JOIN rb_puzzle p ON p.id = tk.puzzle_id
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.puzzle_id = p.id
        LEFT JOIN rb_round r ON r.id = p.round_id
        LEFT JOIN stats ON stats.ticket_id = tk.id
        LEFT JOIN last_msg ON last_msg.ticket_id = tk.id
        WHERE tk.id = $1",
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
        puzzle: make_puzzle(x.p_id, x.p_slug, x.p_title, x.p_state, x.r_id, x.r_slug, x.r_title),
        msg_count: x.msg_count,
        last_at: x.last_at,
        last_by: x.last_by.map(RbTicketSenderType::from_primitive),
    }))
}

pub async fn get_ticket_thread(
    db_pool: &DbPool,
    ticket_id: i32,
    info: &TicketUserInfo,
) -> Result<Option<TicketThread>, RbInternalError> {
    let Some(ticket) = get_ticket_summary(db_pool, ticket_id, true).await? else {
        return Ok(None);
    };
    let messages = get_ticket_messages(db_pool, ticket_id, info.mod_access).await?;
    let send_block = calc_send_block(
        db_pool,
        ticket_id,
        ticket.state,
        info.member_access,
        Some(1),
    )
    .await?;
    let currency = if info.mod_access {
        ticket.currency_ids()
    } else {
        vec![]
    };

    Ok(Some(TicketThread {
        ticket: Some(ticket),
        messages,
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

#[derive(Deserialize)]
pub struct SendMessageData {
    pub content: String,
    pub content_type: RbContentType,
    pub sender_id: i32,
    pub sender_type: RbTicketSenderType,
    pub cost_id: Option<i32>,
    pub cost_amount: i32,
}

async fn insert_ticket_message(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
    .fetch_one(&mut **tx)
    .await?;

    Ok(result)
}

pub async fn send_ticket_message(
    db_pool: &DbPool,
    ticket_id: i32,
    data: &SendMessageData,
    max_pending: Option<i64>,
) -> Result<Option<TicketMessage>, RbInternalError> {
    let mut tx = db_pool.begin().await?;

    if let Some(max_pending) = max_pending
        && matches!(data.sender_type, RbTicketSenderType::Team)
    {
        sqlx::query!("SELECT FROM rb_ticket WHERE id = $1 FOR UPDATE", ticket_id)
            .execute(&mut *tx)
            .await?;

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
            return Ok(None);
        }
    }

    let result = insert_ticket_message(&mut tx, ticket_id, data).await?;

    tx.commit().await?;

    get_ticket_message(db_pool, result, true).await
}

pub async fn close_ticket(
    db_pool: &DbPool,
    ticket_id: i32,
    actor_id: i32,
    actor_type: RbTicketSenderType,
    message: Option<&SendMessageData>,
) -> Result<bool, RbInternalError> {
    let mut tx = db_pool.begin().await?;

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

    if updated {
        let message_id = if let Some(message) = message {
            Some(insert_ticket_message(&mut tx, ticket_id, message).await?)
        } else {
            None
        };

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
    }

    tx.commit().await?;

    Ok(updated)
}

pub async fn close_puzzle_tickets_on_solve(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
    .fetch_all(&mut **tx)
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
        .execute(&mut **tx)
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
        "SELECT t.team_id, m.cost_id, m.cost_amount
        FROM rb_message m
        JOIN rb_ticket t ON t.id = m.ticket_id
        WHERE m.id = $1 AND t.id = $2
            AND NOT m.unlocked
        FOR UPDATE OF m;",
        message_id,
        ticket_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(info) = info else {
        return Ok(PurchaseTicketMessageResult::Unavailable);
    };

    if info.cost_id.is_some() && needs_pay {
        let result = sqlx::query!(
            "UPDATE rb_team_currency tc
            SET utime_at = NOW(), amount = LEAST(
                tc.amount + (EXTRACT(EPOCH FROM (NOW() - tc.utime_at))::INT / 60) * (c.growth + tc.growth),
                c.max_amount
            ) - $3
            FROM rb_currency c
            WHERE tc.currency_id = c.id AND tc.team_id = $1 AND c.id = $2
                AND c.game_id = (
                    SELECT tm.game_id
                    FROM rb_team_member tm
                    WHERE tm.team_id = $1 AND tm.user_id = $4
                )
                AND ($3 <= 0 OR LEAST(
                    tc.amount + (EXTRACT(EPOCH FROM (NOW() - tc.utime_at))::INT / 60) * (c.growth + tc.growth),
                    c.max_amount
                ) >= $3);",
            info.team_id,
            info.cost_id,
            info.cost_amount,
            user_id
        )
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
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

    // check puzzle cooldown
    let cooldown_ready = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_puzzle p
            JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
            WHERE p.id = $2 AND p.ticket_cooldown >= 0
                AND tp.ctime_at <= NOW() - (p.ticket_cooldown * INTERVAL '1 second')
        );",
        team_id,
        puzzle_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(false);
    if !cooldown_ready {
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

    let message_id = insert_ticket_message(&mut tx, ticket_id, message).await?;

    sqlx::query!(
        "INSERT INTO rb_ticket_operation (ticket_id, action, actor, actor_type, message_id)
        VALUES ($1, $2, $3, $4, $5)",
        ticket_id,
        i16::from(TicketOperationAction::Open),
        message.sender_id,
        i16::from(RbTicketSenderType::Team),
        message_id
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let info = TicketUserInfo {
        ticket_id,
        state: RbTicketState::Open,
        puzzle_id: Some(puzzle_id),
        member_access: true,
        mod_access: false,
        admin_access: false,
    };
    let thread = get_ticket_thread(db_pool, ticket_id, &info)
        .await?
        .ok_or("Opened ticket not found")?;

    Ok(OpenPuzzleTicketResult::Ok(Box::new(thread)))
}

pub async fn get_team_puzzle_tickets(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
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
            EXISTS (
                SELECT 1 FROM rb_puzzle p
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
                WHERE p.id = $2 AND p.ticket_cooldown >= 0
                    AND tp.ctime_at <= NOW() - (p.ticket_cooldown * INTERVAL '1 second')
            ) AS ready,
            (
                SELECT tp.ctime_at + (p.ticket_cooldown * INTERVAL '1 second')
                FROM rb_puzzle p
                JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id AND tp.team_id = $1
                WHERE p.id = $2 AND p.ticket_cooldown >= 0
            ) AS cooldown_till;",
        team_id,
        puzzle_id
    )
    .fetch_one(db_pool)
    .await?;

    let open_block = if has_current_puzzle_open {
        TicketOpenBlock::CurrentPuzzlePending
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

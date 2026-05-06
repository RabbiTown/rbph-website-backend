use num_enum::FromPrimitive;
use serde::{Deserialize, Serialize};
use serde_repr::Serialize_repr;
use time::OffsetDateTime;

use crate::{
    DbPool,
    db::round::RbRoundSimpleData,
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
    }))
}

#[derive(Serialize)]
pub struct TicketAggreInfoTeam {
    id: i32,
    name: String,
    state: RbTeamState,
}

#[derive(Serialize)]
pub struct TicketAggreInfoPuzzle {
    id: i32,
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
    has_locked: bool,
}

impl TicketSummary {
    pub fn id(&self) -> i32 {
        self.id
    }
}

#[derive(Serialize)]
pub struct TicketPerm {
    can_send: bool,
    can_host: bool,
    can_view_locked: bool,
}

impl TicketPerm {
    pub fn new(can_send: bool, can_host: bool, can_view_locked: bool) -> Self {
        Self {
            can_send,
            can_host,
            can_view_locked,
        }
    }
}

#[derive(Serialize)]
pub struct TicketThread {
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket: Option<TicketSummary>,
    messages: Vec<TicketMessage>,
    perm: TicketPerm,
}

impl TicketThread {
    pub fn ticket(&self) -> Option<&TicketSummary> {
        self.ticket.as_ref()
    }

    pub fn messages(&self) -> &[TicketMessage] {
        &self.messages
    }
}

#[derive(Serialize)]
pub struct TicketPuzzleList {
    can_open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    block: Option<TicketOpenBlock>,
    tickets: Vec<TicketSummary>,
}

#[repr(i16)]
#[derive(Serialize_repr)]
pub enum TicketOpenBlock {
    Pending = 1,
    Cooldown = 2,
}

pub async fn get_ticket_messages(
    db_pool: &DbPool,
    ticket_id: i32,
    can_view_locked: bool,
) -> Result<Vec<TicketMessage>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT m.id, m.sender, m.sender_type, m.cost_id, m.cost_amount,
                m.unlocked, m.ctime_at, m.utime_at,
                u.id AS u_id, u.nickname AS u_nickname,
                CASE WHEN ($2 OR unlocked) THEN content ELSE NULL END AS content,
                CASE WHEN ($2 OR unlocked) THEN content_type ELSE NULL END AS content_type
        FROM rb_message m
        JOIN rb_user u ON u.id = m.sender
        WHERE ticket_id = $1
        ORDER BY m.ctime_at DESC",
        ticket_id,
        can_view_locked
    )
    .fetch_all(db_pool)
    .await?
    .into_iter()
    .map(|x| TicketMessage {
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
    .collect();

    Ok(result)
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
) -> Result<i32, RbInternalError> {
    let result = sqlx::query_scalar!(
        "INSERT INTO rb_ticket (state, team_id, puzzle_id)
        VALUES (1, $1, NULL)
        ON CONFLICT (team_id) WHERE puzzle_id IS NULL
        DO UPDATE SET team_id = EXCLUDED.team_id
        RETURNING id;",
        team_id
    )
    .fetch_one(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_dm_ticket_thread(
    db_pool: &DbPool,
    team_id: i32,
    can_view_locked: bool,
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
            perm: TicketPerm::new(true, can_view_locked, can_view_locked),
        });
    };

    let ticket = get_ticket_summary(db_pool, ticket_id, false).await?;
    let messages = get_ticket_messages(db_pool, ticket_id, can_view_locked).await?;

    Ok(TicketThread {
        ticket,
        messages,
        perm: TicketPerm::new(true, can_view_locked, can_view_locked),
    })
}

pub async fn get_ticket_summary(
    db_pool: &DbPool,
    ticket_id: i32,
    include_team: bool,
) -> Result<Option<TicketSummary>, RbInternalError> {
    let info = sqlx::query!(
        "WITH stats AS (
            SELECT m.ticket_id, COUNT(*) AS msg_count, MAX(m.ctime_at) AS last_at,
                BOOL_OR(NOT m.unlocked) AS has_locked
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
                p.id AS \"p_id?\", p.title AS \"p_title?\", tp.state AS \"p_state?\",
                r.id AS \"r_id?\", r.title AS \"r_title?\",
                stats.msg_count, stats.last_at, stats.has_locked,
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

    Ok(info.map(|x| TicketSummary {
        id: ticket_id,
        state: RbTicketState::from_primitive(x.state),
        game_id: Some(x.g_id),
        team: include_team.then_some(TicketAggreInfoTeam {
            id: x.t_id,
            name: x.t_name,
            state: RbTeamState::from_primitive(x.t_state),
        }),
        puzzle: make_puzzle(x.p_id, x.p_title, x.p_state, x.r_id, x.r_title),
        msg_count: x.msg_count,
        last_at: x.last_at,
        last_by: x.last_by.map(RbTicketSenderType::from_primitive),
        has_locked: x.has_locked.unwrap_or(false),
    }))
}

pub async fn get_ticket_thread(
    db_pool: &DbPool,
    ticket_id: i32,
    info: &TicketUserInfo,
) -> Result<Option<TicketThread>, RbInternalError> {
    let Some(ticket) = get_ticket_summary(db_pool, ticket_id, info.mod_access).await? else {
        return Ok(None);
    };
    let messages = get_ticket_messages(db_pool, ticket_id, info.mod_access).await?;

    Ok(Some(TicketThread {
        ticket: Some(ticket),
        messages,
        perm: TicketPerm::new(
            !matches!(info.state, RbTicketState::Closed) && info.member_access,
            info.mod_access,
            info.mod_access,
        ),
    }))
}

fn make_puzzle(
    id: Option<i32>,
    title: Option<String>,
    state: Option<i16>,
    round_id: Option<i32>,
    round_title: Option<String>,
) -> Option<TicketAggreInfoPuzzle> {
    match (id, title, state, round_id, round_title) {
        (Some(id), Some(title), Some(state), Some(round_id), Some(round_title)) => {
            Some(TicketAggreInfoPuzzle {
                id,
                title,
                state: RbTeamPuzzleState::from_primitive(state),
                round: RbRoundSimpleData {
                    id: round_id,
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
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(get_ticket_message(db_pool, result, true).await?)
}

pub enum OpenPuzzleTicketResult {
    Ok(TicketThread),
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

    let message_id = sqlx::query_scalar!(
        "INSERT INTO rb_message
            (content, content_type, sender, sender_type,
            cost_id, cost_amount, unlocked, ticket_id)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id;",
        message.content,
        i16::from(message.content_type),
        message.sender_id,
        i16::from(message.sender_type),
        message.cost_id,
        message.cost_amount,
        message.cost_id.is_none(),
        ticket_id
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    let info = TicketUserInfo {
        ticket_id,
        state: RbTicketState::Open,
        puzzle_id: Some(puzzle_id),
        member_access: true,
        mod_access: false,
    };
    let thread = get_ticket_thread(db_pool, ticket_id, &info)
        .await?
        .ok_or("Opened ticket not found")?;

    debug_assert!(thread.messages.iter().any(|msg| msg.id == message_id));

    Ok(OpenPuzzleTicketResult::Ok(thread))
}

pub async fn get_team_puzzle_tickets(
    db_pool: &DbPool,
    team_id: i32,
    puzzle_id: i32,
) -> Result<TicketPuzzleList, RbInternalError> {
    let info = sqlx::query!(
        "WITH stats AS (
            SELECT m.ticket_id, COUNT(*) AS msg_count, MAX(m.ctime_at) AS last_at,
                BOOL_OR(NOT m.unlocked) AS has_locked
            FROM rb_message m
            GROUP BY m.ticket_id
        ),
        last_msg AS (
            SELECT DISTINCT ON (m.ticket_id) m.ticket_id, m.sender_type
            FROM rb_message m
            ORDER BY m.ticket_id, m.ctime_at DESC, m.id DESC
        )
        SELECT tk.id, tk.state, stats.msg_count, stats.last_at, stats.has_locked,
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

    let has_open = info
        .iter()
        .any(|x| x.state == i16::from(RbTicketState::Open));
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
    .fetch_one(db_pool)
    .await?
    .unwrap_or(false);

    let block = if has_open {
        Some(TicketOpenBlock::Pending)
    } else if !cooldown_ready {
        Some(TicketOpenBlock::Cooldown)
    } else {
        None
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
            has_locked: x.has_locked.unwrap_or(false),
        })
        .collect();

    Ok(TicketPuzzleList {
        can_open: block.is_none(),
        block,
        tickets,
    })
}

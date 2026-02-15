use num_enum::FromPrimitive;
use serde::Serialize;
use time::OffsetDateTime;

use crate::{
    DbPool,
    error::RbInternalError,
    model::{
        game::{RbContentType, RbTeamPuzzleState, RbTeamState, RbTicketSenderType, RbTicketState},
        user::RbUserRole,
    },
};

#[derive(Clone)]
pub struct TicketUserInfo {
    pub ticket_id: i32,
    pub team_id: i32,
    pub puzzle_id: Option<i32>,
    pub member_access: bool,
    pub mod_access: bool,
}

impl TicketUserInfo {
    pub fn any_access(&self) -> bool {
        self.member_access || self.mod_access
    }
}

pub async fn get_ticket_user_info(
    db_pool: &DbPool,
    ticket_id: i32,
    user_id: i32,
) -> Result<Option<TicketUserInfo>, RbInternalError> {
    let result = sqlx::query!(
        "SELECT t.team_id, t.puzzle_id, u.urole,
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
        team_id: x.team_id,
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
}

#[derive(Serialize)]
pub struct TicketAggreInfoUser {
    id: i32,
    nickname: String,
}

#[derive(Serialize)]
pub struct TicketMessageInfo {
    id: i32,
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

#[derive(Serialize)]
pub struct TicketAggreInfo {
    id: i32,
    state: RbTicketState,
    team: TicketAggreInfoTeam,
    #[serde(skip_serializing_if = "Option::is_none")]
    puzzle: Option<TicketAggreInfoPuzzle>,
    messages: Vec<TicketMessageInfo>,
}

pub async fn get_dm_ticket_aggre_info(
    db_pool: &DbPool,
    team_id: i32,
    only_unlocked: bool,
) -> Result<Option<TicketAggreInfo>, RbInternalError> {
    let ticket_id = sqlx::query_scalar!(
        "SELECT id FROM rb_ticket
        WHERE team_id = $1 AND puzzle_id IS NULL;",
        team_id
    )
    .fetch_optional(db_pool)
    .await?;

    if let Some(ticket_id) = ticket_id {
        get_ticket_aggre_info(db_pool, ticket_id, only_unlocked).await
    } else {
        Ok(None)
    }
}

pub async fn get_ticket_aggre_info(
    db_pool: &DbPool,
    ticket_id: i32,
    only_unlocked: bool,
) -> Result<Option<TicketAggreInfo>, RbInternalError> {
    let info = sqlx::query!(
        "SELECT tk.state,
                t.id AS t_id, t.tname AS t_name, t.state AS t_state,
                p.id AS \"p_id?\", p.title AS \"p_title?\", tp.state AS \"p_state?\"
        FROM rb_ticket tk
        JOIN rb_team t ON t.id = tk.team_id
        LEFT JOIN rb_puzzle p ON p.id = tk.puzzle_id
        LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.puzzle_id = p.id
        WHERE tk.id = $1",
        ticket_id
    )
    .fetch_optional(db_pool)
    .await?;

    if info.is_none() {
        return Ok(None);
    }
    let info = info.unwrap();

    let puzzle = match (info.p_id, info.p_title, info.p_state) {
        (Some(id), Some(title), Some(state)) => Some(TicketAggreInfoPuzzle {
            id,
            title,
            state: RbTeamPuzzleState::from_primitive(state),
        }),
        _ => None,
    };

    let messages = sqlx::query!(
        "SELECT m.id, m.sender, m.sender_type, m.cost_id, m.cost_amount,
                m.unlocked, m.ctime_at, m.utime_at,
                u.id AS u_id, u.nickname AS u_nickname,
                CASE WHEN ($2 OR unlocked) THEN content ELSE NULL END AS content,
                CASE WHEN ($2 OR unlocked) THEN content_type ELSE NULL END AS content_type
        FROM rb_message m
        JOIN rb_user u ON u.id = m.sender
        WHERE ticket_id = $1",
        ticket_id,
        only_unlocked
    )
    .fetch_all(db_pool)
    .await?
    .into_iter()
    .map(|x| TicketMessageInfo {
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

    Ok(Some(TicketAggreInfo {
        id: ticket_id,
        state: RbTicketState::from_primitive(info.state),
        team: TicketAggreInfoTeam {
            id: info.t_id,
            name: info.t_name,
            state: RbTeamState::from_primitive(info.t_state),
        },
        puzzle,
        messages,
    }))
}

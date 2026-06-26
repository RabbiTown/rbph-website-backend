use num_enum::{FromPrimitive, IntoPrimitive};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[repr(i16)]
#[derive(Clone, Copy, FromPrimitive, IntoPrimitive)]
pub enum NotificationKind {
    TicketReply = 1,

    #[num_enum(catch_all)]
    Unknown(i16),
}

#[derive(Serialize)]
pub struct NotificationActor {
    id: i32,
    nickname: String,
}

#[derive(Serialize)]
pub struct TeamNotification {
    id: i64,
    kind: i16,
    actor: Option<NotificationActor>,
    data: Value,
    read: bool,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    read_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    ctime_at: OffsetDateTime,
}

pub async fn list_for_team(
    pool: &DbPool,
    team_id: i32,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<TeamNotification>, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT n.id, n.kind, n.data, n.read_at, n.ctime_at,
            u.id AS \"actor_id?\", u.nickname AS \"actor_nickname?\"
        FROM rb_notification n
        LEFT JOIN rb_user u ON u.id = n.actor
        WHERE n.team_id = $1 AND ($2::BIGINT IS NULL OR n.id < $2)
        ORDER BY n.id DESC
        LIMIT $3",
        team_id,
        before,
        limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| TeamNotification {
            id: row.id,
            kind: row.kind,
            actor: row
                .actor_id
                .zip(row.actor_nickname)
                .map(|(id, nickname)| NotificationActor { id, nickname }),
            data: row.data,
            read: row.read_at.is_some(),
            read_at: row.read_at,
            ctime_at: row.ctime_at,
        })
        .collect())
}

pub async fn get_for_team(
    pool: &DbPool,
    team_id: i32,
    notification_id: i64,
) -> Result<Option<TeamNotification>, RbInternalError> {
    let row = sqlx::query!(
        "SELECT n.id, n.kind, n.data, n.read_at, n.ctime_at,
            u.id AS \"actor_id?\", u.nickname AS \"actor_nickname?\"
        FROM rb_notification n
        LEFT JOIN rb_user u ON u.id = n.actor
        WHERE n.team_id = $1 AND n.id = $2",
        team_id,
        notification_id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| TeamNotification {
        id: row.id,
        kind: row.kind,
        actor: row
            .actor_id
            .zip(row.actor_nickname)
            .map(|(id, nickname)| NotificationActor { id, nickname }),
        data: row.data,
        read: row.read_at.is_some(),
        read_at: row.read_at,
        ctime_at: row.ctime_at,
    }))
}

pub struct NotificationUnreadCount {
    pub count: i64,
    pub dm_count: i64,
}

pub async fn unread_count(
    pool: &DbPool,
    team_id: i32,
) -> Result<NotificationUnreadCount, RbInternalError> {
    let row = sqlx::query!(
        r#"SELECT
            COUNT(*) AS "count!",
            COUNT(*) FILTER (WHERE data->>'puzzle_id' IS NULL) AS "dm_count!"
        FROM rb_notification
        WHERE team_id = $1 AND read_at IS NULL"#,
        team_id,
    )
    .fetch_one(pool)
    .await?;

    Ok(NotificationUnreadCount {
        count: row.count,
        dm_count: row.dm_count,
    })
}

pub async fn mark_read(
    pool: &DbPool,
    team_id: i32,
    notification_id: i64,
) -> Result<bool, RbInternalError> {
    Ok(sqlx::query_scalar!(
        "UPDATE rb_notification SET read_at = NOW()
        WHERE id = $1 AND team_id = $2 AND read_at IS NULL
        RETURNING id",
        notification_id,
        team_id,
    )
    .fetch_optional(pool)
    .await?
    .is_some())
}

pub async fn mark_many_read(
    pool: &DbPool,
    team_id: i32,
    notification_ids: &[i64],
) -> Result<bool, RbInternalError> {
    if notification_ids.is_empty() {
        return Ok(false);
    }

    Ok(sqlx::query!(
        "UPDATE rb_notification SET read_at = NOW()
        WHERE team_id = $1
            AND id = ANY($2)
            AND read_at IS NULL",
        team_id,
        notification_ids,
    )
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

pub async fn mark_ticket_messages_read(
    pool: &DbPool,
    team_id: i32,
    message_ids: &[i32],
) -> Result<bool, RbInternalError> {
    if message_ids.is_empty() {
        return Ok(false);
    }

    Ok(sqlx::query!(
        "UPDATE rb_notification SET read_at = NOW()
        WHERE team_id = $1
            AND kind = $2
            AND source_id = ANY($3)
            AND read_at IS NULL",
        team_id,
        i16::from(NotificationKind::TicketReply),
        message_ids,
    )
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

pub async fn mark_all_read(pool: &DbPool, team_id: i32) -> Result<bool, RbInternalError> {
    Ok(sqlx::query!(
        "UPDATE rb_notification SET read_at = NOW()
        WHERE team_id = $1 AND read_at IS NULL",
        team_id,
    )
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

pub struct NotificationSyncInfo {
    pub id: i64,
    pub team_id: i32,
    pub game_id: i32,
}

pub async fn get_sync_info_by_source(
    pool: &DbPool,
    kind: NotificationKind,
    source_id: i32,
) -> Result<Option<NotificationSyncInfo>, RbInternalError> {
    Ok(sqlx::query!(
        "SELECT n.id, n.team_id, t.game_id
        FROM rb_notification n
        JOIN rb_team t ON t.id = n.team_id
        WHERE n.kind = $1 AND n.source_id = $2",
        i16::from(kind),
        source_id,
    )
    .fetch_optional(pool)
    .await?
    .map(|row| NotificationSyncInfo {
        id: row.id,
        team_id: row.team_id,
        game_id: row.game_id,
    }))
}

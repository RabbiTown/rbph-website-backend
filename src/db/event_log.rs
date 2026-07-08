use num_enum::IntoPrimitive;
use serde::Serialize;
use serde_json::{Value, json};
use serde_repr::Serialize_repr;
use sqlx::PgConnection;
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[repr(i16)]
#[derive(Clone, Copy, IntoPrimitive, Serialize_repr)]
pub enum EventScope {
    TeamActivity = 1,
    System = 2,
    Admin = 3,
    Security = 4,
}

#[repr(i16)]
#[derive(Clone, Copy, IntoPrimitive, Serialize_repr)]
pub enum EventSeverity {
    Info = 0,
    Warning = 1,
    Error = 2,
}

#[derive(Default)]
pub struct EventLogInput {
    pub event_type: &'static str,
    pub event_scope: i16,
    pub severity: i16,
    pub game_id: Option<i32>,
    pub team_id: Option<i32>,
    pub user_id: Option<i32>,
    pub target_user_id: Option<i32>,
    pub puzzle_id: Option<i32>,
    pub round_id: Option<i32>,
    pub hint_id: Option<i32>,
    pub ticket_id: Option<i32>,
    pub submission_id: Option<i32>,
    pub currency_id: Option<i32>,
    pub delta_amount: Option<i64>,
    pub data: Value,
}

pub async fn insert_pool(pool: &DbPool, event: EventLogInput) -> Result<i64, RbInternalError> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO rb_event_log (
            event_type, event_scope, severity, game_id, team_id, user_id, target_user_id,
            puzzle_id, round_id, hint_id, ticket_id, submission_id, currency_id, delta_amount, data
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        RETURNING id;"#,
        event.event_type,
        event.event_scope,
        event.severity,
        event.game_id,
        event.team_id,
        event.user_id,
        event.target_user_id,
        event.puzzle_id,
        event.round_id,
        event.hint_id,
        event.ticket_id,
        event.submission_id,
        event.currency_id,
        event.delta_amount,
        event.data
    )
    .fetch_one(pool)
    .await?;

    Ok(id)
}

pub async fn insert_conn(
    conn: &mut PgConnection,
    event: EventLogInput,
) -> Result<i64, RbInternalError> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO rb_event_log (
            event_type, event_scope, severity, game_id, team_id, user_id, target_user_id,
            puzzle_id, round_id, hint_id, ticket_id, submission_id, currency_id, delta_amount, data
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        RETURNING id;"#,
        event.event_type,
        event.event_scope,
        event.severity,
        event.game_id,
        event.team_id,
        event.user_id,
        event.target_user_id,
        event.puzzle_id,
        event.round_id,
        event.hint_id,
        event.ticket_id,
        event.submission_id,
        event.currency_id,
        event.delta_amount,
        event.data
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(id)
}

#[derive(Serialize)]
pub struct EventLogData {
    pub id: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub scope: i16,
    pub severity: i16,
    pub game_id: Option<i32>,
    pub team_id: Option<i32>,
    pub user_id: Option<i32>,
    pub target_user_id: Option<i32>,
    pub puzzle_id: Option<i32>,
    pub round_id: Option<i32>,
    pub hint_id: Option<i32>,
    pub ticket_id: Option<i32>,
    pub submission_id: Option<i32>,
    pub currency_id: Option<i32>,
    pub delta_amount: Option<i64>,
    pub data: Value,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub ctime_at: OffsetDateTime,
}

#[derive(Serialize)]
pub struct CurrencyActivitySummary {
    pub currency_id: i32,
    pub init_amount: i64,
    pub current_amount: i64,
    pub logged_delta: i64,
}

pub async fn list_team_activity(
    pool: &DbPool,
    team_id: i32,
    currency_id: Option<i32>,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<EventLogData>, RbInternalError> {
    let scope = i16::from(EventScope::TeamActivity);
    let limit = limit.clamp(1, 100);
    let rows = sqlx::query_as!(
        EventLogData,
        r#"SELECT el.id, el.event_type, el.event_scope AS scope, el.severity, el.game_id, el.team_id, el.user_id,
            el.target_user_id, el.puzzle_id, el.round_id, el.hint_id, el.ticket_id, el.submission_id, el.currency_id,
            el.delta_amount,
            (
                (el.data - 'user' - 'target_user')
                || CASE
                    WHEN u.id IS NULL THEN '{}'::JSONB
                    ELSE jsonb_build_object('user', jsonb_build_object('id', u.id, 'nickname', u.nickname))
                END
                || CASE
                    WHEN tu.id IS NULL THEN '{}'::JSONB
                    ELSE jsonb_build_object('target_user', jsonb_build_object('id', tu.id, 'nickname', tu.nickname))
                END
            ) AS "data!",
            CASE
                WHEN p.id IS NULL THEN el.ctime_at
                ELSE GREATEST(el.ctime_at, rp.release_at)
            END AS "ctime_at!"
        FROM rb_event_log el
        LEFT JOIN rb_user u ON u.id = el.user_id
        LEFT JOIN rb_user tu ON tu.id = el.target_user_id
        LEFT JOIN rb_puzzle p ON p.id = el.puzzle_id
        LEFT JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        WHERE el.team_id = $1
            AND el.event_scope = $2
            AND (
                ($3::INT IS NULL AND el.event_type != 'currency.penalty')
                OR ($3::INT IS NOT NULL AND el.currency_id = $3)
            )
            AND (p.id IS NULL OR rp.release_at <= NOW())
            AND ($4::BIGINT IS NULL OR el.id < $4)
        ORDER BY el.id DESC
        LIMIT $5;"#,
        team_id,
        scope,
        currency_id,
        before,
        limit
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub struct AdminLogQuery<'a> {
    pub scope: Option<i16>,
    pub severity: Option<i16>,
    pub event_type: Option<&'a str>,
    pub game_id: Option<i32>,
    pub team_id: Option<i32>,
    pub user_id: Option<i32>,
    pub offset: i64,
    pub limit: i64,
}

pub async fn list_admin_logs(
    pool: &DbPool,
    query: AdminLogQuery<'_>,
) -> Result<Vec<EventLogData>, RbInternalError> {
    let limit = query.limit.clamp(1, 100);
    let offset = query.offset.max(0);
    let rows = sqlx::query_as!(
        EventLogData,
        r#"SELECT el.id, el.event_type, el.event_scope AS scope, el.severity, el.game_id, el.team_id, el.user_id,
            el.target_user_id, el.puzzle_id, el.round_id, el.hint_id, el.ticket_id, el.submission_id, el.currency_id,
            el.delta_amount,
            (
                (el.data - 'user' - 'target_user')
                || CASE
                    WHEN u.id IS NULL THEN '{}'::JSONB
                    ELSE jsonb_build_object('user', jsonb_build_object('id', u.id, 'nickname', u.nickname))
                END
                || CASE
                    WHEN tu.id IS NULL THEN '{}'::JSONB
                    ELSE jsonb_build_object('target_user', jsonb_build_object('id', tu.id, 'nickname', tu.nickname))
                END
            ) AS "data!",
            el.ctime_at
        FROM rb_event_log el
        LEFT JOIN rb_user u ON u.id = el.user_id
        LEFT JOIN rb_user tu ON tu.id = el.target_user_id
        WHERE ($1::SMALLINT IS NULL OR el.event_scope = $1)
            AND ($2::SMALLINT IS NULL OR el.severity = $2)
            AND ($3::TEXT IS NULL OR el.event_type = $3)
            AND ($4::INT IS NULL OR el.game_id = $4)
            AND ($5::INT IS NULL OR el.team_id = $5)
            AND ($6::INT IS NULL OR el.user_id = $6 OR el.target_user_id = $6)
        ORDER BY el.id DESC
        LIMIT $7
        OFFSET $8;"#,
        query.scope,
        query.severity,
        query.event_type,
        query.game_id,
        query.team_id,
        query.user_id,
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

pub async fn count_admin_logs(
    pool: &DbPool,
    scope: Option<i16>,
    severity: Option<i16>,
    event_type: Option<&str>,
    game_id: Option<i32>,
    team_id: Option<i32>,
    user_id: Option<i32>,
) -> Result<i64, RbInternalError> {
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
        FROM rb_event_log el
        WHERE ($1::SMALLINT IS NULL OR el.event_scope = $1)
            AND ($2::SMALLINT IS NULL OR el.severity = $2)
            AND ($3::TEXT IS NULL OR el.event_type = $3)
            AND ($4::INT IS NULL OR el.game_id = $4)
            AND ($5::INT IS NULL OR el.team_id = $5)
            AND ($6::INT IS NULL OR el.user_id = $6 OR el.target_user_id = $6);"#,
        scope,
        severity,
        event_type,
        game_id,
        team_id,
        user_id
    )
    .fetch_one(pool)
    .await?;

    Ok(count)
}

pub async fn get_currency_activity_summary(
    pool: &DbPool,
    team_id: i32,
    currency_id: i32,
) -> Result<Option<CurrencyActivitySummary>, RbInternalError> {
    let row = sqlx::query!(
        r#"SELECT
            c.id AS "currency_id!",
            c.init_amount,
            LEAST(
                tc.amount::NUMERIC + FLOOR(EXTRACT(EPOCH FROM (NOW() - tc.utime_at)) / 60) * (c.growth + tc.growth)::NUMERIC,
                c.max_amount::NUMERIC
            )::BIGINT AS "current_amount!",
            COALESCE(SUM(el.delta_amount), 0)::BIGINT AS "logged_delta!"
        FROM rb_currency c
        JOIN rb_team_currency tc ON tc.currency_id = c.id AND tc.team_id = $1
        LEFT JOIN rb_event_log el ON el.team_id = tc.team_id
            AND el.currency_id = c.id
        WHERE c.id = $2
        GROUP BY c.id, c.init_amount, tc.amount, tc.utime_at, tc.growth, c.growth, c.max_amount;"#,
        team_id,
        currency_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| CurrencyActivitySummary {
        currency_id: row.currency_id,
        init_amount: row.init_amount,
        current_amount: row.current_amount,
        logged_delta: row.logged_delta,
    }))
}

#[derive(Clone)]
pub struct CurrencyEventData {
    pub id: i32,
    pub slug: String,
    pub name: String,
    pub prec: i32,
    pub before: i64,
    pub after: i64,
}

impl CurrencyEventData {
    pub fn delta(&self) -> i64 {
        self.after - self.before
    }

    pub fn json(
        &self,
        reason: Option<&str>,
        puzzle_id: Option<i32>,
        puzzle_title: Option<&str>,
    ) -> Value {
        json!({
            "reason": reason,
            "puzzle": puzzle_id.map(|id| json!({
                "id": id,
                "title": puzzle_title
            })),
            "currency": {
                "id": self.id,
                "slug": self.slug,
                "name": self.name,
                "prec": self.prec
            },
            "delta": self.delta(),
            "before": self.before,
            "after": self.after
        })
    }
}

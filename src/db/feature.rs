use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameFeature {
    TeamFormation,
    DirectMessage,
    PuzzleTicket,
    Leaderboard,
    Currency,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameFeatureState {
    Closed,
    ExistingOnly,
    Open,
    Live,
    Locked,
}

impl GameFeature {
    pub const ALL: [Self; 5] = [
        Self::TeamFormation,
        Self::DirectMessage,
        Self::PuzzleTicket,
        Self::Leaderboard,
        Self::Currency,
    ];

    pub const fn value(self) -> i16 {
        match self {
            Self::TeamFormation => 0,
            Self::DirectMessage => 1,
            Self::PuzzleTicket => 2,
            Self::Leaderboard => 3,
            Self::Currency => 4,
        }
    }

    pub const fn default_state(self) -> GameFeatureState {
        match self {
            Self::TeamFormation | Self::DirectMessage | Self::PuzzleTicket => {
                GameFeatureState::Open
            }
            Self::Leaderboard => GameFeatureState::Live,
            Self::Currency => GameFeatureState::Closed,
        }
    }

    pub fn states(self) -> Vec<GameFeatureState> {
        match self {
            Self::TeamFormation => vec![GameFeatureState::Closed, GameFeatureState::Open],
            Self::DirectMessage | Self::PuzzleTicket => vec![
                GameFeatureState::Closed,
                GameFeatureState::ExistingOnly,
                GameFeatureState::Open,
            ],
            Self::Leaderboard => vec![GameFeatureState::Live, GameFeatureState::Locked],
            Self::Currency => vec![GameFeatureState::Closed, GameFeatureState::Open],
        }
    }

    pub fn encode_state(self, state: GameFeatureState) -> Option<i16> {
        match (self, state) {
            (Self::TeamFormation, GameFeatureState::Closed) => Some(0),
            (Self::TeamFormation, GameFeatureState::Open) => Some(1),
            (Self::DirectMessage | Self::PuzzleTicket, GameFeatureState::Closed) => Some(0),
            (Self::DirectMessage | Self::PuzzleTicket, GameFeatureState::ExistingOnly) => Some(1),
            (Self::DirectMessage | Self::PuzzleTicket, GameFeatureState::Open) => Some(2),
            (Self::Leaderboard, GameFeatureState::Live) => Some(0),
            (Self::Leaderboard, GameFeatureState::Locked) => Some(1),
            (Self::Currency, GameFeatureState::Closed) => Some(0),
            (Self::Currency, GameFeatureState::Open) => Some(1),
            _ => None,
        }
    }

    pub fn decode_state(self, value: i16) -> Option<GameFeatureState> {
        match (self, value) {
            (Self::TeamFormation, 0) => Some(GameFeatureState::Closed),
            (Self::TeamFormation, 1) => Some(GameFeatureState::Open),
            (Self::DirectMessage | Self::PuzzleTicket, 0) => Some(GameFeatureState::Closed),
            (Self::DirectMessage | Self::PuzzleTicket, 1) => Some(GameFeatureState::ExistingOnly),
            (Self::DirectMessage | Self::PuzzleTicket, 2) => Some(GameFeatureState::Open),
            (Self::Leaderboard, 0) => Some(GameFeatureState::Live),
            (Self::Leaderboard, 1) => Some(GameFeatureState::Locked),
            (Self::Currency, 0) => Some(GameFeatureState::Closed),
            (Self::Currency, 1) => Some(GameFeatureState::Open),
            _ => None,
        }
    }

    pub fn from_value(value: i16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|feature| feature.value() == value)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct FeatureChangeData {
    pub feature: GameFeature,
    pub state: GameFeatureState,
}

#[derive(Serialize)]
pub struct AdminFeatureData {
    pub feature: GameFeature,
    pub state: GameFeatureState,
    pub states: Vec<GameFeatureState>,
    pub source_phase_id: Option<i32>,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub updated_at: OffsetDateTime,
    pub next_change: Option<AdminFeatureNextChange>,
}

#[derive(Serialize)]
pub struct AdminFeatureNextChange {
    pub phase_id: i32,
    pub phase_title: String,
    pub state: GameFeatureState,
    #[serde(with = "crate::serde_helpers::serialize_offset_datetime")]
    pub release_at: OffsetDateTime,
}

pub fn valid_changes(changes: &[FeatureChangeData]) -> bool {
    changes
        .iter()
        .all(|change| change.feature.encode_state(change.state).is_some())
        && changes.iter().enumerate().all(|(index, change)| {
            !changes[..index]
                .iter()
                .any(|previous| previous.feature == change.feature)
        })
}

pub async fn initialize_game_tx(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
) -> Result<(), RbInternalError> {
    for feature in GameFeature::ALL {
        sqlx::query!(
            "INSERT INTO rb_game_feature (game_id, feature_type, state)
            VALUES ($1, $2, $3);",
            game_id,
            feature.value(),
            feature
                .encode_state(feature.default_state())
                .ok_or("Invalid default feature state")?
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn list_admin(
    pool: &DbPool,
    game_id: i32,
) -> Result<Vec<AdminFeatureData>, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT gf.feature_type, gf.state, gf.source_phase_id, gf.utime_at,
            next.phase_id AS \"phase_id?\", next.phase_title AS \"phase_title?\",
            next.target_state AS \"target_state?\", next.release_at AS \"release_at?\"
        FROM rb_game_feature gf
        LEFT JOIN LATERAL (
            SELECT c.phase_id, rp.title AS phase_title, c.target_state, rp.release_at
            FROM rb_release_phase_feature_change c
            JOIN rb_release_phase rp ON rp.id = c.phase_id
            WHERE c.game_id = gf.game_id AND c.feature_type = gf.feature_type
                AND rp.release_at > NOW()
            ORDER BY rp.release_at, rp.id
            LIMIT 1
        ) next ON TRUE
        WHERE gf.game_id = $1
        ORDER BY gf.feature_type;",
        game_id
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let feature =
                GameFeature::from_value(row.feature_type).ok_or("Invalid game feature")?;
            let state = feature
                .decode_state(row.state)
                .ok_or("Invalid game feature state")?;
            let next_change = match (
                row.phase_id,
                row.phase_title,
                row.target_state,
                row.release_at,
            ) {
                (Some(phase_id), Some(phase_title), Some(target_state), Some(release_at)) => {
                    Some(AdminFeatureNextChange {
                        phase_id,
                        phase_title,
                        state: feature
                            .decode_state(target_state)
                            .ok_or("Invalid planned feature state")?,
                        release_at,
                    })
                }
                _ => None,
            };
            Ok(AdminFeatureData {
                feature,
                state,
                states: feature.states(),
                source_phase_id: row.source_phase_id,
                updated_at: row.utime_at,
                next_change,
            })
        })
        .collect()
}

pub async fn player_states(
    pool: &DbPool,
    game_id: i32,
) -> Result<BTreeMap<GameFeature, GameFeatureState>, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT feature_type, state FROM rb_game_feature
        WHERE game_id = $1 ORDER BY feature_type;",
        game_id
    )
    .fetch_all(pool)
    .await?;
    let mut result = BTreeMap::new();
    for row in rows {
        let feature = GameFeature::from_value(row.feature_type).ok_or("Invalid game feature")?;
        result.insert(
            feature,
            feature
                .decode_state(row.state)
                .ok_or("Invalid game feature state")?,
        );
    }
    Ok(result)
}

async fn lock_leaderboard_tx(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
    phase_id: Option<i32>,
    locked_at: OffsetDateTime,
) -> Result<(), RbInternalError> {
    let inserted = sqlx::query_scalar!(
        "INSERT INTO rb_leaderboard_lock (game_id, phase_id, locked_at)
        VALUES ($1, $2, $3)
        ON CONFLICT (game_id) DO NOTHING
        RETURNING game_id;",
        game_id,
        phase_id,
        locked_at
    )
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_none() {
        return Ok(());
    }

    sqlx::query!(
        "INSERT INTO rb_leaderboard_lock_team (
            game_id, team_id, rank, solves, finish_at, last_solved_at
        )
        SELECT $1, ranked.id, ranked.rank::INT, ranked.solves,
            ranked.finish_at, ranked.last_solved_at
        FROM (
            SELECT t.id, t.finish_at,
                COUNT(tp.puzzle_id) AS solves,
                MAX(tp.solve_at) AS last_solved_at,
                ROW_NUMBER() OVER (ORDER BY (t.finish_at IS NULL), t.finish_at ASC NULLS LAST,
                    COUNT(tp.puzzle_id) DESC, MAX(tp.solve_at) ASC NULLS LAST, t.id) AS rank
            FROM rb_team t
            LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.state = 1
            WHERE t.game_id = $1 AND t.is_locked AND NOT t.is_banned
            GROUP BY t.id
        ) ranked;",
        game_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn settle_currency_growth_tx(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
    currency_id: Option<i32>,
    effective_at: OffsetDateTime,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        r#"UPDATE rb_team_currency tc
        SET amount = GREATEST(
                0::NUMERIC,
                LEAST(
                    tc.amount::NUMERIC
                        + GREATEST(
                            FLOOR(EXTRACT(EPOCH FROM ($3 - tc.utime_at)) / 60),
                            0::NUMERIC
                        ) * (c.growth + tc.growth)::NUMERIC,
                    c.max_amount::NUMERIC
                )
            )::BIGINT,
            utime_at = GREATEST(tc.utime_at, $3)
        FROM rb_currency c
        WHERE tc.currency_id = c.id
            AND c.game_id = $1
            AND ($2::INT IS NULL OR c.id = $2);"#,
        game_id,
        currency_id,
        effective_at
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn enable_currency_growth_tx(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
    effective_at: OffsetDateTime,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "UPDATE rb_team_currency tc
        SET utime_at = GREATEST(tc.utime_at, $2)
        FROM rb_currency c
        WHERE tc.currency_id = c.id AND c.game_id = $1;",
        game_id,
        effective_at
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn set_state_tx(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
    change: &FeatureChangeData,
    phase_id: Option<i32>,
    actor_id: Option<i32>,
    effective_at: OffsetDateTime,
) -> Result<bool, RbInternalError> {
    let target = change
        .feature
        .encode_state(change.state)
        .ok_or("Invalid game feature state")?;
    if let Some(phase_id) = phase_id
        && sqlx::query_scalar!(
            "SELECT EXISTS (
                SELECT 1 FROM rb_game_feature_history
                WHERE phase_id = $1 AND feature_type = $2
            ) AS \"exists!\";",
            phase_id,
            change.feature.value()
        )
        .fetch_one(&mut **tx)
        .await?
    {
        return Ok(false);
    }

    let old = sqlx::query_scalar!(
        "SELECT state FROM rb_game_feature
        WHERE game_id = $1 AND feature_type = $2
        FOR UPDATE;",
        game_id,
        change.feature.value()
    )
    .fetch_one(&mut **tx)
    .await?;

    if matches!(change.feature, GameFeature::Leaderboard) {
        match change.state {
            GameFeatureState::Locked => {
                lock_leaderboard_tx(tx, game_id, phase_id, effective_at).await?;
            }
            GameFeatureState::Live => {
                sqlx::query!(
                    "DELETE FROM rb_leaderboard_lock WHERE game_id = $1;",
                    game_id
                )
                .execute(&mut **tx)
                .await?;
            }
            _ => return Err("Invalid leaderboard state".into()),
        }
    }

    if old != target && matches!(change.feature, GameFeature::Currency) {
        match change.state {
            GameFeatureState::Closed => {
                settle_currency_growth_tx(tx, game_id, None, effective_at).await?;
            }
            GameFeatureState::Open => {
                enable_currency_growth_tx(tx, game_id, effective_at).await?;
            }
            _ => return Err("Invalid currency state".into()),
        }
    }

    sqlx::query!(
        "UPDATE rb_game_feature
        SET state = $3, source_phase_id = $4, utime_at = $5
        WHERE game_id = $1 AND feature_type = $2;",
        game_id,
        change.feature.value(),
        target,
        phase_id,
        effective_at
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO rb_game_feature_history (
            game_id, feature_type, old_state, new_state, phase_id, actor_id, effective_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7);",
        game_id,
        change.feature.value(),
        old,
        target,
        phase_id,
        actor_id,
        effective_at
    )
    .execute(&mut **tx)
    .await?;
    Ok(old != target || matches!(change.feature, GameFeature::Leaderboard))
}

pub async fn apply_phase_changes(
    pool: &DbPool,
    game_id: i32,
    phase_id: i32,
    effective_at: OffsetDateTime,
) -> Result<bool, RbInternalError> {
    let rows = sqlx::query!(
        "SELECT feature_type, target_state
        FROM rb_release_phase_feature_change
        WHERE phase_id = $1 ORDER BY feature_type;",
        phase_id
    )
    .fetch_all(pool)
    .await?;
    let mut tx = pool.begin().await?;
    let mut leaderboard_changed = false;
    for row in rows {
        let feature = GameFeature::from_value(row.feature_type).ok_or("Invalid game feature")?;
        let change = FeatureChangeData {
            feature,
            state: feature
                .decode_state(row.target_state)
                .ok_or("Invalid game feature state")?,
        };
        let changed = set_state_tx(
            &mut tx,
            game_id,
            &change,
            Some(phase_id),
            None,
            effective_at,
        )
        .await?;
        leaderboard_changed |= changed && matches!(feature, GameFeature::Leaderboard);
    }
    tx.commit().await?;
    Ok(leaderboard_changed)
}

pub async fn set_manual_state(
    pool: &DbPool,
    game_id: i32,
    change: &FeatureChangeData,
    actor_id: i32,
) -> Result<Option<bool>, RbInternalError> {
    if !valid_changes(std::slice::from_ref(change)) {
        return Ok(None);
    }
    let mut tx = pool.begin().await?;
    let exists = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM rb_game WHERE id = $1);",
        game_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    let changed = set_state_tx(
        &mut tx,
        game_id,
        change,
        None,
        Some(actor_id),
        OffsetDateTime::now_utc(),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(changed))
}

#[cfg(test)]
mod tests {
    use super::{FeatureChangeData, GameFeature, GameFeatureState, valid_changes};

    #[test]
    fn validates_feature_state_pairs_and_duplicates() {
        assert!(valid_changes(&[FeatureChangeData {
            feature: GameFeature::DirectMessage,
            state: GameFeatureState::ExistingOnly,
        }]));
        assert!(!valid_changes(&[FeatureChangeData {
            feature: GameFeature::Leaderboard,
            state: GameFeatureState::Open,
        }]));
        assert!(valid_changes(&[FeatureChangeData {
            feature: GameFeature::Currency,
            state: GameFeatureState::Open,
        }]));
        assert!(!valid_changes(&[FeatureChangeData {
            feature: GameFeature::Currency,
            state: GameFeatureState::Live,
        }]));
        assert!(!valid_changes(&[
            FeatureChangeData {
                feature: GameFeature::TeamFormation,
                state: GameFeatureState::Open,
            },
            FeatureChangeData {
                feature: GameFeature::TeamFormation,
                state: GameFeatureState::Closed,
            },
        ]));
    }
}

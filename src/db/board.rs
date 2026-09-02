use std::{collections::HashMap, sync::Arc, time::Duration};

use deadpool_redis::redis::AsyncCommands;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, pool::PoolConnection};
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::{DbPool, error::RbInternalError, kv::KvStore};

const SNAPSHOT_TTL_SECONDS: u64 = 120;
const LOCK_NAMESPACE: i32 = 737_001;
const MAIN_BOARD_TYPE: &str = "main";

#[derive(Clone, Deserialize, Serialize)]
pub struct LeaderBoardTeamInfo {
    pub rank: usize,
    pub id: i32,
    pub name: String,
    pub bio: String,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub finish_at: Option<OffsetDateTime>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub last_solved_at: Option<OffsetDateTime>,
    pub solves: i64,
    pub members: Vec<String>,
}

#[derive(Serialize)]
pub struct LeaderBoardInfo {
    pub data: Vec<LeaderBoardTeamInfo>,
    pub version: i64,
    pub total: usize,
    pub has_more: bool,
    pub reset: bool,
    pub state: &'static str,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    pub locked_at: Option<OffsetDateTime>,
}

struct LeaderBoardTeamRow {
    id: i32,
    name: String,
    bio: String,
    finish_at: Option<OffsetDateTime>,
    last_solved_at: Option<OffsetDateTime>,
    solves: i64,
}

#[derive(Clone, Deserialize, Serialize)]
struct LeaderBoardSnapshot {
    order: Vec<i32>,
    teams: HashMap<i32, LeaderBoardTeamInfo>,
    #[serde(with = "crate::serde_helpers::serialize_option_offset_datetime")]
    locked_at: Option<OffsetDateTime>,
    version: i64,
}

pub struct LeaderBoardCache {
    cache: RwLock<HashMap<i32, Arc<LeaderBoardSnapshot>>>,
}

pub static LEADER_BOARD_CACHE: Lazy<LeaderBoardCache> = Lazy::new(|| LeaderBoardCache {
    cache: RwLock::new(HashMap::new()),
});

fn latest_key(kv: &KvStore, game_id: i32, board_type: &str) -> String {
    kv.key(format!(
        "cache:leaderboard:v1:{game_id}:{board_type}:latest"
    ))
}

fn snapshot_key(kv: &KvStore, game_id: i32, board_type: &str, version: i64) -> String {
    kv.key(format!(
        "cache:leaderboard:v1:{game_id}:{board_type}:snapshot:{version}"
    ))
}

impl LeaderBoardCache {
    async fn fetch_teams(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<Vec<LeaderBoardTeamInfo>, RbInternalError> {
        let locked = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM rb_leaderboard_lock WHERE game_id = $1) AS \"locked!\";",
            game_id
        )
        .fetch_one(db_pool)
        .await?;

        let teams: Vec<LeaderBoardTeamRow> = if locked {
            sqlx::query!(
                "SELECT t.id, t.name, t.bio, s.finish_at, s.last_solved_at, s.solves
                FROM rb_leaderboard_lock_team s
                JOIN rb_team t ON t.id = s.team_id
                LEFT JOIN rb_team_feature tf
                    ON tf.team_id = t.id AND tf.feature_type = 3
                WHERE s.game_id = $1
                    AND NOT t.is_banned
                    AND COALESCE(tf.enabled, TRUE)
                ORDER BY s.rank, t.id;",
                game_id
            )
            .fetch_all(db_pool)
            .await?
            .into_iter()
            .map(|team| LeaderBoardTeamRow {
                id: team.id,
                name: team.name,
                bio: team.bio,
                finish_at: team.finish_at,
                last_solved_at: team.last_solved_at,
                solves: team.solves,
            })
            .collect()
        } else {
            sqlx::query!(
                "SELECT t.id, t.name, t.bio, t.finish_at, MAX(tp.solve_at) AS last_solved_at,
                    COUNT(tp.puzzle_id)::BIGINT AS \"solves!\"
                FROM rb_team t
                LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.state = 1
                LEFT JOIN rb_team_feature tf
                    ON tf.team_id = t.id AND tf.feature_type = 3
                WHERE t.game_id = $1
                    AND t.is_locked
                    AND NOT t.is_banned
                    AND COALESCE(tf.enabled, TRUE)
                GROUP BY t.id
                ORDER BY (t.finish_at IS NULL),
                    t.finish_at ASC NULLS LAST,
                    COUNT(tp.puzzle_id) DESC,
                    MAX(tp.solve_at) ASC NULLS LAST,
                    t.id ASC;",
                game_id
            )
            .fetch_all(db_pool)
            .await?
            .into_iter()
            .map(|team| LeaderBoardTeamRow {
                id: team.id,
                name: team.name,
                bio: team.bio,
                finish_at: team.finish_at,
                last_solved_at: team.last_solved_at,
                solves: team.solves,
            })
            .collect()
        };

        let members = sqlx::query!(
            "SELECT tm.team_id, u.nickname
            FROM rb_team_member tm
            JOIN rb_user u ON u.id = tm.user_id
            WHERE tm.game_id = $1
            ORDER BY tm.is_captain DESC, tm.ctime_at ASC;",
            game_id
        )
        .fetch_all(db_pool)
        .await?;

        let mut team_members: HashMap<i32, Vec<String>> = HashMap::new();
        for el in members {
            team_members
                .entry(el.team_id)
                .or_default()
                .push(el.nickname);
        }

        Ok(teams
            .into_iter()
            .map(|team| LeaderBoardTeamInfo {
                rank: 0,
                id: team.id,
                name: team.name,
                bio: team.bio,
                finish_at: team.finish_at,
                last_solved_at: team.last_solved_at,
                solves: team.solves,
                members: team_members.get(&team.id).cloned().unwrap_or_default(),
            })
            .collect())
    }

    async fn build_snapshot(
        &self,
        db_pool: &DbPool,
        game_id: i32,
        version: i64,
    ) -> Result<LeaderBoardSnapshot, RbInternalError> {
        let data = self.fetch_teams(db_pool, game_id).await?;
        let locked_at = sqlx::query_scalar!(
            "SELECT locked_at FROM rb_leaderboard_lock WHERE game_id = $1;",
            game_id
        )
        .fetch_optional(db_pool)
        .await?;

        Ok(LeaderBoardSnapshot {
            order: data.iter().map(|x| x.id).collect(),
            teams: data.into_iter().map(|x| (x.id, x)).collect(),
            locked_at,
            version,
        })
    }

    async fn publish_snapshot(
        &self,
        kv_pool: &KvStore,
        game_id: i32,
        snapshot: LeaderBoardSnapshot,
    ) -> Result<Arc<LeaderBoardSnapshot>, RbInternalError> {
        let snapshot = Arc::new(snapshot);
        let payload = serde_json::to_string(&*snapshot)?;
        let mut conn = kv_pool.get().await?;
        let _: () = conn
            .set_ex(
                snapshot_key(kv_pool, game_id, MAIN_BOARD_TYPE, snapshot.version),
                &payload,
                SNAPSHOT_TTL_SECONDS,
            )
            .await?;
        let _: () = conn
            .set(latest_key(kv_pool, game_id, MAIN_BOARD_TYPE), &payload)
            .await?;

        self.cache.write().await.insert(game_id, snapshot.clone());
        Ok(snapshot)
    }

    async fn load_snapshot(
        &self,
        kv_pool: &KvStore,
        game_id: i32,
        version: Option<i64>,
    ) -> Result<Option<Arc<LeaderBoardSnapshot>>, RbInternalError> {
        let key = match version {
            Some(version) => snapshot_key(kv_pool, game_id, MAIN_BOARD_TYPE, version),
            None => latest_key(kv_pool, game_id, MAIN_BOARD_TYPE),
        };
        let mut conn = kv_pool.get().await?;
        let payload: Option<String> = conn.get(key).await?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let snapshot: LeaderBoardSnapshot = serde_json::from_str(&payload)?;
        let snapshot = Arc::new(snapshot);
        if version.is_none() {
            self.cache.write().await.insert(game_id, snapshot.clone());
        }
        Ok(Some(snapshot))
    }

    async fn try_advisory_lock(
        &self,
        conn: &mut PoolConnection<Postgres>,
        game_id: i32,
    ) -> Result<bool, RbInternalError> {
        Ok(sqlx::query_scalar!(
            "SELECT pg_try_advisory_lock($1, $2) AS \"locked!\";",
            LOCK_NAMESPACE,
            game_id
        )
        .fetch_one(&mut **conn)
        .await?)
    }

    async fn advisory_unlock(
        &self,
        conn: &mut PoolConnection<Postgres>,
        game_id: i32,
    ) -> Result<(), RbInternalError> {
        let _: bool = sqlx::query_scalar!(
            "SELECT pg_advisory_unlock($1, $2) AS \"unlocked!\";",
            LOCK_NAMESPACE,
            game_id
        )
        .fetch_one(&mut **conn)
        .await?;
        Ok(())
    }

    async fn next_version(&self, db_pool: &DbPool, game_id: i32) -> Result<i64, RbInternalError> {
        let version = sqlx::query_scalar!(
            "INSERT INTO rb_leaderboard_refresh_state (game_id, board_type, next_version)
            VALUES ($1, $2, 1)
            ON CONFLICT (game_id, board_type) DO UPDATE SET
                next_version = rb_leaderboard_refresh_state.next_version + 1,
                full_rebuild = FALSE,
                updated_at = CURRENT_TIMESTAMP
            RETURNING next_version;",
            game_id,
            MAIN_BOARD_TYPE,
        )
        .fetch_one(db_pool)
        .await?;
        Ok(version)
    }

    async fn rebuild_and_publish(
        &self,
        db_pool: &DbPool,
        kv_pool: &KvStore,
        game_id: i32,
    ) -> Result<Option<Arc<LeaderBoardSnapshot>>, RbInternalError> {
        let mut conn = db_pool.acquire().await?;
        if !self.try_advisory_lock(&mut conn, game_id).await? {
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Ok(Some(snapshot)) = self.load_snapshot(kv_pool, game_id, None).await {
                    return Ok(Some(snapshot));
                }
            }
            return Ok(self.cache.read().await.get(&game_id).cloned());
        }

        let result = async {
            let version = self.next_version(db_pool, game_id).await?;
            let snapshot = self.build_snapshot(db_pool, game_id, version).await?;
            match self
                .publish_snapshot(kv_pool, game_id, snapshot.clone())
                .await
            {
                Ok(snapshot) => Ok(snapshot),
                Err(error) => {
                    log::error!(
                        "failed to publish cold leaderboard snapshot for game {game_id}: {error}"
                    );
                    let snapshot = Arc::new(snapshot);
                    self.cache.write().await.insert(game_id, snapshot.clone());
                    Ok(snapshot)
                }
            }
        }
        .await;
        let unlock_result = self.advisory_unlock(&mut conn, game_id).await;
        match (result, unlock_result) {
            (Ok(snapshot), Ok(())) => Ok(Some(snapshot)),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    pub async fn refresh_game(
        &self,
        db_pool: &DbPool,
        kv_pool: &KvStore,
        game_id: i32,
    ) -> Result<(), RbInternalError> {
        let mut conn = db_pool.acquire().await?;
        if !self.try_advisory_lock(&mut conn, game_id).await? {
            return Ok(());
        }

        let result = async {
            let max_revision = sqlx::query_scalar!(
                "SELECT MAX(revision) FROM rb_leaderboard_dirty_team
                WHERE game_id = $1 AND board_type = $2;",
                game_id,
                MAIN_BOARD_TYPE,
            )
            .fetch_one(db_pool)
            .await?;
            let version = self.next_version(db_pool, game_id).await?;
            let snapshot = self.build_snapshot(db_pool, game_id, version).await?;
            self.publish_snapshot(kv_pool, game_id, snapshot).await?;
            if let Some(max_revision) = max_revision {
                sqlx::query!(
                    "DELETE FROM rb_leaderboard_dirty_team
                    WHERE game_id = $1 AND board_type = $2 AND revision <= $3;",
                    game_id,
                    MAIN_BOARD_TYPE,
                    max_revision,
                )
                .execute(db_pool)
                .await?;
            }
            sqlx::query!(
                "UPDATE rb_leaderboard_refresh_state
                SET full_rebuild = FALSE, updated_at = CURRENT_TIMESTAMP
                WHERE game_id = $1 AND board_type = $2;",
                game_id,
                MAIN_BOARD_TYPE,
            )
            .execute(db_pool)
            .await?;
            Ok::<_, RbInternalError>(())
        }
        .await;
        let unlock_result = self.advisory_unlock(&mut conn, game_id).await;
        match (result, unlock_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
        }
    }

    pub async fn dirty_games(&self, db_pool: &DbPool) -> Result<Vec<i32>, RbInternalError> {
        Ok(sqlx::query_scalar!(
            "SELECT q.game_id AS \"game_id!\"
            FROM (
                SELECT DISTINCT game_id FROM rb_leaderboard_dirty_team WHERE board_type = 'main'
                UNION
                SELECT game_id FROM rb_leaderboard_refresh_state WHERE board_type = 'main' AND full_rebuild
            ) q
            ORDER BY q.game_id;"
        )
        .fetch_all(db_pool)
        .await?)
    }

    pub async fn mark_team_dirty(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        affects_order: bool,
    ) -> Result<(), RbInternalError> {
        sqlx::query!(
            "WITH target AS (
                SELECT game_id, id AS team_id FROM rb_team WHERE id = $1
            ), state AS (
                INSERT INTO rb_leaderboard_refresh_state (game_id, board_type)
                SELECT game_id, $2 FROM target
                ON CONFLICT (game_id, board_type) DO NOTHING
            )
            INSERT INTO rb_leaderboard_dirty_team (game_id, board_type, team_id, affects_order)
            SELECT game_id, $2, team_id, $3 FROM target
            ON CONFLICT (game_id, board_type, team_id) DO UPDATE SET
                revision = nextval('rb_leaderboard_dirty_revision_seq'),
                affects_order = rb_leaderboard_dirty_team.affects_order OR EXCLUDED.affects_order,
                updated_at = CURRENT_TIMESTAMP;",
            team_id,
            MAIN_BOARD_TYPE,
            affects_order,
        )
        .execute(db_pool)
        .await?;
        Ok(())
    }

    pub async fn mark_game_dirty(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<(), RbInternalError> {
        sqlx::query!(
            "INSERT INTO rb_leaderboard_refresh_state (game_id, board_type, full_rebuild)
            VALUES ($1, $2, TRUE)
            ON CONFLICT (game_id, board_type) DO UPDATE SET
                full_rebuild = TRUE,
                updated_at = CURRENT_TIMESTAMP;",
            game_id,
            MAIN_BOARD_TYPE,
        )
        .execute(db_pool)
        .await?;
        self.cache.write().await.remove(&game_id);
        Ok(())
    }

    pub async fn update_team(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        affects_order: bool,
    ) -> Result<(), RbInternalError> {
        self.mark_team_dirty(db_pool, team_id, affects_order).await
    }

    pub async fn remove_team(&self, db_pool: &DbPool, game_id: i32) -> Result<(), RbInternalError> {
        self.mark_game_dirty(db_pool, game_id).await
    }

    pub async fn invalidate_game(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<(), RbInternalError> {
        self.mark_game_dirty(db_pool, game_id).await
    }

    pub async fn get_info(
        &self,
        db_pool: &DbPool,
        kv_pool: &KvStore,
        game_id: i32,
        prev_version: Option<i64>,
        offset: usize,
        limit: usize,
    ) -> Result<Option<LeaderBoardInfo>, RbInternalError> {
        let mut reset = false;
        let mut snapshot = if offset > 0 {
            match prev_version {
                Some(version) => self
                    .load_snapshot(kv_pool, game_id, Some(version))
                    .await
                    .unwrap_or_else(|error| {
                        log::error!(
                            "failed to load leaderboard snapshot for game {game_id}: {error}"
                        );
                        None
                    }),
                None => None,
            }
        } else {
            None
        };

        if snapshot.is_none() {
            reset = offset > 0;
            snapshot = self
                .load_snapshot(kv_pool, game_id, None)
                .await
                .unwrap_or_else(|error| {
                    log::error!(
                        "failed to load latest leaderboard snapshot for game {game_id}: {error}"
                    );
                    None
                });
        }

        if snapshot.is_none() {
            snapshot = self.rebuild_and_publish(db_pool, kv_pool, game_id).await?;
        }

        let snapshot = if let Some(snapshot) = snapshot {
            snapshot
        } else if let Some(snapshot) = self.cache.read().await.get(&game_id).cloned() {
            snapshot
        } else {
            return Ok(None);
        };

        if offset == 0 && prev_version == Some(snapshot.version) {
            return Ok(None);
        }

        let page_offset = if reset { 0 } else { offset };
        let total = snapshot.order.len();
        let teams = snapshot
            .order
            .iter()
            .enumerate()
            .skip(page_offset)
            .take(limit)
            .filter_map(|(index, id)| {
                snapshot.teams.get(id).cloned().map(|mut team| {
                    team.rank = index + 1;
                    team
                })
            })
            .collect();

        Ok(Some(LeaderBoardInfo {
            data: teams,
            version: snapshot.version,
            total,
            has_more: page_offset.saturating_add(limit) < total,
            reset,
            state: if snapshot.locked_at.is_some() {
                "locked"
            } else {
                "live"
            },
            locked_at: snapshot.locked_at,
        }))
    }
}

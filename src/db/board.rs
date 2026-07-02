use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Serialize)]
pub struct LeaderBoardTeamInfo {
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
    pub version: u32,
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

struct LeaderBoard {
    order: Vec<i32>,
    teams: HashMap<i32, LeaderBoardTeamInfo>,

    order_dirty: bool,
    json_cache: Option<String>,
    locked_at: Option<OffsetDateTime>,

    pub version: u32,
}

impl LeaderBoard {
    fn mark_order_dirty(&mut self) {
        self.order_dirty = true;
        self.json_cache = None;
    }

    fn mark_order_clean(&mut self) {
        self.order_dirty = false;
    }

    fn mark_json_dirty(&mut self) {
        self.json_cache = None;
    }

    fn bump_version(&mut self) {
        self.version += 1;
    }
}

pub struct LeaderBoardCache {
    cache: RwLock<HashMap<i32, LeaderBoard>>,
}

pub static LEADER_BOARD_CACHE: Lazy<LeaderBoardCache> = Lazy::new(|| LeaderBoardCache {
    cache: RwLock::new(HashMap::new()),
});

impl LeaderBoardCache {
    async fn fetch_order(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<Vec<i32>, RbInternalError> {
        let locked = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM rb_leaderboard_lock WHERE game_id = $1) AS \"locked!\";",
            game_id
        )
        .fetch_one(db_pool)
        .await?;
        if locked {
            return Ok(sqlx::query_scalar!(
                "SELECT rb_leaderboard_lock_team.team_id FROM rb_leaderboard_lock_team
                JOIN rb_team t ON t.id = rb_leaderboard_lock_team.team_id
                LEFT JOIN rb_team_feature tf
                    ON tf.team_id = t.id AND tf.feature_type = 3
                WHERE rb_leaderboard_lock_team.game_id = $1
                    AND NOT t.is_banned
                    AND COALESCE(tf.enabled, TRUE)
                ORDER BY rank;",
                game_id
            )
            .fetch_all(db_pool)
            .await?);
        }
        let result = sqlx::query_scalar!(
            "SELECT t.id
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
                finish_at ASC NULLS LAST,
                COUNT(tp.puzzle_id) DESC,
                MAX(tp.solve_at) ASC NULLS LAST;",
            game_id
        )
        .fetch_all(db_pool)
        .await?;

        Ok(result)
    }

    pub async fn update_order(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<(), RbInternalError> {
        let new_order = self.fetch_order(db_pool, game_id).await?;

        let mut guard = self.cache.write().await;
        let cache = guard.get_mut(&game_id).ok_or("Not Found")?;
        cache.order = new_order;

        cache.mark_order_clean();
        cache.bump_version();

        Ok(())
    }

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
                ORDER BY s.rank;",
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
                    COUNT(tp.puzzle_id) AS \"solves!\"
                FROM rb_team t
                LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.state = 1
                LEFT JOIN rb_team_feature tf
                    ON tf.team_id = t.id AND tf.feature_type = 3
                WHERE t.game_id = $1
                    AND t.is_locked
                    AND NOT t.is_banned
                    AND COALESCE(tf.enabled, TRUE)
                GROUP BY t.id
                ORDER BY (t.finish_at IS NULL), finish_at ASC NULLS LAST,
                    \"solves!\" DESC, MAX(tp.solve_at) ASC NULLS LAST;",
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

        let result = teams
            .into_iter()
            .map(|team| {
                let LeaderBoardTeamRow {
                    id,
                    name,
                    bio,
                    finish_at,
                    last_solved_at,
                    solves,
                } = team;
                LeaderBoardTeamInfo {
                    id,
                    name,
                    bio,
                    finish_at,
                    last_solved_at,
                    solves,
                    members: team_members.get(&id).cloned().unwrap_or_default(),
                }
            })
            .collect();

        Ok(result)
    }

    pub async fn update_all(&self, db_pool: &DbPool, game_id: i32) -> Result<(), RbInternalError> {
        let data = self.fetch_teams(db_pool, game_id).await?;
        let locked_at = sqlx::query_scalar!(
            "SELECT locked_at FROM rb_leaderboard_lock WHERE game_id = $1;",
            game_id
        )
        .fetch_optional(db_pool)
        .await?;

        let new_order = data.iter().map(|x| x.id).collect();
        let new_teams = data.into_iter().map(|x| (x.id, x)).collect();

        let mut guard = self.cache.write().await;
        guard.insert(
            game_id,
            LeaderBoard {
                order: new_order,
                teams: new_teams,
                order_dirty: false,
                json_cache: None,
                locked_at,
                version: 0,
            },
        );

        Ok(())
    }

    async fn fetch_team(
        &self,
        db_pool: &DbPool,
        team_id: i32,
    ) -> Result<Option<(i32, LeaderBoardTeamInfo)>, RbInternalError> {
        let locked_team = sqlx::query!(
            "SELECT s.game_id, t.id, t.name, t.bio, s.finish_at,
                s.last_solved_at, s.solves
            FROM rb_leaderboard_lock_team s
            JOIN rb_team t ON t.id = s.team_id
            LEFT JOIN rb_team_feature tf
                ON tf.team_id = t.id AND tf.feature_type = 3
            WHERE s.team_id = $1 AND NOT t.is_banned AND COALESCE(tf.enabled, TRUE);",
            team_id
        )
        .fetch_optional(db_pool)
        .await?;
        if let Some(team) = locked_team {
            let members = sqlx::query_scalar!(
                "SELECT u.nickname FROM rb_team_member tm
                JOIN rb_user u ON u.id = tm.user_id
                WHERE tm.team_id = $1
                ORDER BY tm.is_captain DESC, tm.ctime_at ASC;",
                team_id
            )
            .fetch_all(db_pool)
            .await?;
            return Ok(Some((
                team.game_id,
                LeaderBoardTeamInfo {
                    id: team.id,
                    bio: team.bio,
                    name: team.name,
                    finish_at: team.finish_at,
                    last_solved_at: team.last_solved_at,
                    solves: team.solves,
                    members,
                },
            )));
        }
        let team = sqlx::query!(
            "SELECT t.id, t.name, t.bio, t.finish_at, MAX(tp.solve_at) AS last_solved_at,
                COUNT(tp.puzzle_id) AS \"solves!\", t.game_id
            FROM rb_team t
            LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.state = 1
            LEFT JOIN rb_team_feature tf
                ON tf.team_id = t.id AND tf.feature_type = 3
            WHERE t.id = $1 AND t.is_locked AND NOT t.is_banned AND COALESCE(tf.enabled, TRUE)
            GROUP BY t.id;",
            team_id
        )
        .fetch_optional(db_pool)
        .await?;

        if team.is_none() {
            return Ok(None);
        }
        let team = team.unwrap();

        let members = sqlx::query_scalar!(
            "SELECT u.nickname
            FROM rb_team_member tm
            JOIN rb_user u ON u.id = tm.user_id
            WHERE tm.team_id = $1
            ORDER BY tm.is_captain DESC, tm.ctime_at ASC;",
            team_id
        )
        .fetch_all(db_pool)
        .await?;

        Ok(Some((
            team.game_id,
            LeaderBoardTeamInfo {
                id: team.id,
                bio: team.bio.clone(),
                name: team.name,
                finish_at: team.finish_at,
                last_solved_at: team.last_solved_at,
                solves: team.solves,
                members,
            },
        )))
    }

    pub async fn update_team(
        &self,
        db_pool: &DbPool,
        team_id: i32,
        affact_order: bool,
    ) -> Result<(), RbInternalError> {
        if let Some((game_id, team)) = self.fetch_team(db_pool, team_id).await? {
            let mut guard = self.cache.write().await;
            if let Some(cache) = guard.get_mut(&game_id) {
                if affact_order {
                    cache.mark_order_dirty();
                } else {
                    cache.mark_json_dirty();
                }
                cache.teams.insert(team.id, team);
                cache.bump_version();
            }
        }

        Ok(())
    }

    pub async fn remove_team(&self, game_id: i32, team_id: i32) -> Result<(), RbInternalError> {
        let mut guard = self.cache.write().await;
        if let Some(cache) = guard.get_mut(&game_id) {
            if cache.teams.remove(&team_id).is_some() {
                cache.mark_order_dirty();
            } else {
                cache.mark_json_dirty();
            }
        }

        Ok(())
    }

    pub async fn invalidate_game(&self, game_id: i32) {
        self.cache.write().await.remove(&game_id);
    }

    // TODO : use scheduled update (5-10s maybe)
    pub async fn get_info_str(
        &self,
        db_pool: &DbPool,
        game_id: i32,
        prev_version: Option<u32>,
    ) -> Result<Option<String>, RbInternalError> {
        // check if anything needs updates (R LOCK)
        let (needs_all, needs_order, version) = {
            let guard = self.cache.read().await;
            match guard.get(&game_id) {
                None => (true, false, 0),
                Some(cache) => (false, cache.order_dirty, cache.version),
            }
        };

        if needs_all {
            self.update_all(db_pool, game_id).await?;
        } else if needs_order {
            self.update_order(db_pool, game_id).await?;
        } else if Some(version) == prev_version {
            return Ok(None);
        }

        // get json cache (R LOCK)
        let (version, locked_at) = {
            let guard = self.cache.read().await;
            let leaderboard = guard.get(&game_id).ok_or("Not Found")?;

            if let Some(result) = &leaderboard.json_cache {
                return Ok(Some(result.clone()));
            }

            (leaderboard.version, leaderboard.locked_at)
        };

        let teams_vec: Vec<LeaderBoardTeamInfo> = {
            let guard = self.cache.read().await;
            let leaderboard = guard.get(&game_id).ok_or("Not Found")?;
            leaderboard
                .order
                .iter()
                .filter_map(|id| leaderboard.teams.get(id))
                .cloned()
                .collect()
        };

        let info = LeaderBoardInfo {
            data: teams_vec,
            version,
            state: if locked_at.is_some() {
                "locked"
            } else {
                "live"
            },
            locked_at,
        };

        let result = serde_json::to_string(&info)?;

        // update json cache (W LOCK)
        {
            let mut guard = self.cache.write().await;
            if let Some(cache) = guard.get_mut(&game_id) {
                cache.json_cache = Some(result.clone());
            }
        }

        Ok(Some(result))
    }
}

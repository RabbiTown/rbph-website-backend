use std::collections::HashMap;

use once_cell::sync::Lazy;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::sync::RwLock;

use crate::{DbPool, error::RbInternalError};

#[derive(Clone, Serialize)]
pub struct LeaderBoardTeamInfo {
    pub id: i32,
    pub tname: String,
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
}

struct LeaderBoard {
    order: Vec<i32>,
    teams: HashMap<i32, LeaderBoardTeamInfo>,

    order_dirty: bool,
    json_cache: Option<String>,
}

impl LeaderBoard {
    fn mark_order_dirty(&mut self) {
        self.order_dirty = true;
        self.json_cache = None;
    }

    fn mark_order_clean(&mut self) {
        self.order_dirty = false;
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
        let result = sqlx::query_scalar!(
            "SELECT t.id
            FROM rb_team t
            LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.pstate = 1
            WHERE t.game_id = $1 AND t.tstate > 0
            GROUP BY t.id
            ORDER BY t.tstate DESC,
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

        Ok(())
    }

    async fn fetch_teams(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<Vec<LeaderBoardTeamInfo>, RbInternalError> {
        let teams = sqlx::query!(
            "SELECT t.id, t.tname, t.bio, t.finish_at, MAX(tp.solve_at) AS last_solved_at,
                COUNT(tp.puzzle_id) AS \"solves!\"
            FROM rb_team t
            LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.pstate = 1
            WHERE t.game_id = $1 AND t.tstate > 0
            GROUP BY t.id
            ORDER BY t.tstate DESC,
                finish_at ASC NULLS LAST,
                \"solves!\" DESC,
                MAX(tp.solve_at) ASC NULLS LAST;",
            game_id
        )
        .fetch_all(db_pool)
        .await?;

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
            .iter()
            .map(|t| LeaderBoardTeamInfo {
                id: t.id,
                bio: t.bio.clone(),
                tname: t.tname.clone(),
                finish_at: t.finish_at,
                last_solved_at: t.last_solved_at,
                solves: t.solves,
                members: team_members.get(&t.id).cloned().unwrap_or_default(),
            })
            .collect();

        Ok(result)
    }

    pub async fn update_all(&self, db_pool: &DbPool, game_id: i32) -> Result<(), RbInternalError> {
        let data = self.fetch_teams(db_pool, game_id).await?;

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
            },
        );

        Ok(())
    }

    async fn fetch_team(
        &self,
        db_pool: &DbPool,
        team_id: i32,
    ) -> Result<(i32, LeaderBoardTeamInfo), RbInternalError> {
        let team = sqlx::query!(
            "SELECT t.id, t.tname, t.bio, t.finish_at, MAX(tp.solve_at) AS last_solved_at,
                COUNT(tp.puzzle_id) AS \"solves!\", t.game_id
            FROM rb_team t
            LEFT JOIN rb_team_puzzle tp ON tp.team_id = t.id AND tp.pstate = 1
            WHERE t.id = $1
            GROUP BY t.id;",
            team_id
        )
        .fetch_one(db_pool)
        .await?;

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

        Ok((
            team.game_id,
            LeaderBoardTeamInfo {
                id: team.id,
                bio: team.bio.clone(),
                tname: team.tname,
                finish_at: team.finish_at,
                last_solved_at: team.last_solved_at,
                solves: team.solves,
                members,
            },
        ))
    }

    pub async fn update_team(&self, db_pool: &DbPool, team_id: i32) -> Result<(), RbInternalError> {
        let (game_id, team) = self.fetch_team(db_pool, team_id).await?;

        let mut guard = self.cache.write().await;
        if let Some(cache) = guard.get_mut(&game_id) {
            cache.mark_order_dirty();
            cache.teams.insert(team.id, team);
        }

        Ok(())
    }

    // TODO : use scheduled update (5-10s maybe)
    pub async fn get_info_str(
        &self,
        db_pool: &DbPool,
        game_id: i32,
    ) -> Result<String, RbInternalError> {
        // check if anything needs updates (R LOCK)
        let (needs_all, needs_order) = {
            let guard = self.cache.read().await;
            match guard.get(&game_id) {
                None => (true, false),
                Some(cache) => (false, cache.order_dirty),
            }
        };

        if needs_all {
            self.update_all(db_pool, game_id).await?;
        } else if needs_order {
            self.update_order(db_pool, game_id).await?;
        }

        // get json cache (R LOCK)
        {
            let guard = self.cache.read().await;
            let leaderboard = guard.get(&game_id).ok_or("Not Found")?;

            if let Some(result) = &leaderboard.json_cache {
                return Ok(result.clone());
            }
        }

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

        let info = LeaderBoardInfo { data: teams_vec };

        let result = serde_json::to_string(&info)?;

        // update json cache (W LOCK)
        {
            let mut guard = self.cache.write().await;
            if let Some(cache) = guard.get_mut(&game_id) {
                cache.json_cache = Some(result.clone());
            }
        }

        Ok(result)
    }
}

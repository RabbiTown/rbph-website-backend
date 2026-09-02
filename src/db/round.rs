use serde::{Deserialize, Serialize};

use crate::{
    AppState, DbPool,
    db::{self, game::GameUserInfo, puzzle::RbPuzzleTeamStateShowData},
    error::RbInternalError,
    model::game::RbTeamPuzzleState,
    model::user::RbUserRole,
};

pub async fn get_round_game(
    db_pool: &DbPool,
    round_id: i32,
) -> Result<Option<i32>, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT game_id FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(result)
}

pub async fn get_round_id_by_game_ref(
    db_pool: &DbPool,
    game_id: i32,
    round_ref: &str,
) -> Result<Option<i32>, RbInternalError> {
    let result = if let Ok(round_id) = round_ref.parse::<i32>() {
        sqlx::query_scalar!(
            "SELECT id FROM rb_round
            WHERE game_id = $1 AND id = $2;",
            game_id,
            round_id
        )
        .fetch_optional(db_pool)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT id FROM rb_round
            WHERE game_id = $1 AND slug = $2;",
            game_id,
            round_ref
        )
        .fetch_optional(db_pool)
        .await?
    };

    Ok(result)
}

pub async fn get_round_state(
    db_pool: &DbPool,
    team_id: i32,
    round_id: i32,
) -> Result<bool, RbInternalError> {
    let result = sqlx::query_scalar!(
        "SELECT EXISTS (
            SELECT 1 FROM rb_team_puzzle tp
            JOIN rb_team t ON t.id = tp.team_id
            JOIN rb_puzzle p ON p.id = tp.puzzle_id AND p.round_id = $2
            JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
            WHERE tp.team_id = $1 AND NOT t.is_banned AND tp.state >= 0
                AND rp.release_at <= NOW()
        );",
        team_id,
        round_id
    )
    .fetch_one(db_pool)
    .await?
    .unwrap_or(false);

    Ok(result)
}

pub async fn get_round_user_info(
    db_pool: &DbPool,
    user_id: i32,
    round_id: i32,
    user_role: RbUserRole,
) -> Result<Option<GameUserInfo>, RbInternalError> {
    let Some(game_id) = get_round_game(db_pool, round_id).await? else {
        return Ok(None);
    };

    let Some(team_id) = db::game::get_game_user_info(db_pool, user_id, game_id, user_role)
        .await?
        .and_then(|info| info.team_id)
    else {
        return Ok(None);
    };

    let access = get_round_state(db_pool, team_id, round_id).await?;

    match access {
        true => Ok(Some(GameUserInfo {
            game_id,
            team_id: Some(team_id),
        })),
        false => Ok(None),
    }
}

#[derive(Serialize)]
pub struct RbRoundShowData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub cover: Option<String>,
    pub game_id: i32,
    pub puzzle: Option<i32>,
}

#[derive(Serialize)]
pub struct RbPuzzleSimpleData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    pub state: RbTeamPuzzleState,
    pub answer: Option<String>,
}

#[derive(Serialize)]
pub struct RbRoundTeamStateShowData {
    pub puzzles: Vec<RbPuzzleSimpleData>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub puzzle: Option<RbPuzzleTeamStateShowData>,
}

pub async fn get_state_for_team(
    db_pool: &DbPool,
    team_id: i32,
    round_id: i32,
) -> Result<RbRoundTeamStateShowData, RbInternalError> {
    let puzzles = sqlx::query_as!(
        RbPuzzleSimpleData,
        "SELECT p.id, p.slug, p.title, tp.state AS state,
                CASE WHEN COUNT(s.id) = 1 THEN MAX(s.real_answer) ELSE NULL END AS answer
        FROM rb_puzzle p
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
        LEFT JOIN rb_submission s ON s.puzzle_id = p.id
            AND s.team_id = tp.team_id
            AND (s.saction = 1 OR s.saction = 5)
            AND s.real_answer IS NOT NULL
        WHERE p.round_id = $1 AND tp.team_id = $2 AND tp.state >= 0
            AND rp.release_at <= NOW()
            AND p.id IS DISTINCT FROM (SELECT puzzle FROM rb_round WHERE id = $1)
        GROUP BY p.id, p.slug, p.title, p.sort, tp.state
        ORDER BY p.sort, p.id;",
        round_id,
        team_id
    )
    .fetch_all(db_pool)
    .await?;

    let row = sqlx::query!(
        "SELECT GREATEST(tp.ctime_at, rp.release_at) AS \"utime_at!\",
                tp.state, tp.cooldown_till,
                tp.max_submit + p.max_submit AS max_submit,
                COUNT(DISTINCT fs.id) AS submit_count,
                ARRAY_AGG(DISTINCT s.real_answer) FILTER (WHERE s.real_answer IS NOT NULL) AS answers
        FROM rb_team_puzzle tp
        JOIN rb_round r ON r.id = $2
        JOIN rb_puzzle p ON p.id = tp.puzzle_id
        JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
        LEFT JOIN rb_submission fs ON fs.puzzle_id = tp.puzzle_id
            AND fs.team_id = tp.team_id
            AND fs.saction = 0
            AND NOT fs.ignored
        LEFT JOIN rb_submission s ON s.puzzle_id = tp.puzzle_id
            AND s.team_id = tp.team_id
            AND s.saction = 1
        WHERE tp.team_id = $1 AND tp.puzzle_id = r.puzzle
            AND tp.state >= 0
            AND rp.release_at <= NOW()
        GROUP BY GREATEST(tp.ctime_at, rp.release_at),
            tp.state, tp.max_submit, tp.cooldown_till, p.max_submit;",
        team_id,
        round_id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(RbRoundTeamStateShowData {
        puzzles,
        puzzle: row.map(|r| RbPuzzleTeamStateShowData {
            state: r.state.into(),
            max_submit: r.max_submit,
            submit_count: r.submit_count.unwrap_or(0),
            answers: r.answers.unwrap_or_default(),
            utime_at: r.utime_at,
            cooldown_till: r.cooldown_till,
        }),
    })
}

#[derive(Serialize)]
pub struct RbRoundForTeamData {
    data: RbRoundShowData,
    state: RbRoundTeamStateShowData,
}

pub async fn get_info_for_team(
    db_pool: &DbPool,
    round_id: i32,
    team_id: i32,
) -> Result<Option<RbRoundForTeamData>, RbInternalError> {
    let data = sqlx::query_as!(
        RbRoundShowData,
        "SELECT id, slug, title, cover, game_id, puzzle
         FROM rb_round
         WHERE id = $1",
        round_id,
    )
    .fetch_optional(db_pool)
    .await?;
    let Some(data) = data else {
        return Ok(None);
    };
    let state = get_state_for_team(db_pool, team_id, round_id).await?;
    Ok(Some(RbRoundForTeamData { data, state }))
}

#[derive(Serialize)]
pub struct RbRoundSimpleData {
    pub id: i32,
    pub slug: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

pub async fn get_simple_list_for_team(
    app: &AppState,
    game_id: i32,
    team_id: i32,
) -> Result<Vec<RbRoundSimpleData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundSimpleData,
        "SELECT r.id, r.slug, r.title, r.description
        FROM rb_round r
        WHERE r.game_id = $1
        AND EXISTS (
            SELECT 1 FROM rb_puzzle p
            JOIN rb_puzzle_effective_release rp ON rp.puzzle_id = p.id
            JOIN rb_team_puzzle tp ON tp.puzzle_id = p.id
                AND tp.team_id = $2 AND tp.state >= 0
            WHERE p.round_id = r.id
                AND rp.release_at <= NOW()
        )
        ORDER BY r.sort, r.id;",
        game_id,
        team_id
    )
    .fetch_all(&app.db)
    .await?;

    Ok(result)
}

#[derive(Serialize)]
pub struct RbRoundAdminData {
    pub id: i32,
    pub slug: Option<String>,
    pub sort: i32,
    pub title: String,
    pub description: Option<String>,
    pub cover: Option<String>,
    pub game_id: i32,
    pub puzzle: Option<i32>,
}

#[derive(Deserialize)]
pub struct RbRoundCreateData {
    pub game_id: i32,
    pub slug: Option<String>,
    #[serde(default)]
    pub sort: i32,
    pub title: String,
    pub description: Option<String>,
    pub content: String,
    #[serde(default)]
    pub content_type: i16,
    pub cover: Option<String>,
    pub puzzle: Option<i32>,
}

#[derive(Default, Deserialize)]
pub struct RbRoundUpdateData {
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub slug: Option<Option<String>>,
    pub sort: Option<i32>,
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_string_patch"
    )]
    pub cover: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_nullable_i32_patch"
    )]
    pub puzzle: Option<Option<i32>>,
}

pub async fn admin_list(
    pool: &DbPool,
    game_id: Option<i32>,
) -> Result<Vec<RbRoundAdminData>, RbInternalError> {
    let result = if let Some(game_id) = game_id {
        sqlx::query_as!(
            RbRoundAdminData,
            "SELECT id, slug, sort, title, description, cover, game_id, puzzle
        FROM rb_round
        WHERE game_id = $1
        ORDER BY sort, id;",
            game_id
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as!(
            RbRoundAdminData,
            "SELECT id, slug, sort, title, description, cover, game_id, puzzle
        FROM rb_round
        ORDER BY game_id, sort, id;",
        )
        .fetch_all(pool)
        .await?
    };

    Ok(result)
}

pub async fn admin_get(
    pool: &DbPool,
    round_id: i32,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let result = sqlx::query_as!(
        RbRoundAdminData,
        "SELECT id, slug, sort, title, description, cover, game_id, puzzle
        FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_create(
    pool: &DbPool,
    data: &RbRoundCreateData,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query_as!(
        RbRoundAdminData,
        "INSERT INTO rb_round (slug, sort, title, description, cover, game_id, puzzle)
        SELECT $2, $3, $4, $5, $6, g.id, NULL::INT
        FROM rb_game g
        WHERE g.id = $1 AND $7::INT IS NULL
        RETURNING id, slug, sort, title, description, cover, game_id, puzzle;",
        data.game_id,
        data.slug,
        data.sort,
        data.title,
        data.description,
        data.cover,
        data.puzzle
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(round) = &result {
        sqlx::query!(
            "INSERT INTO rb_content_block (round_id, sort, name, content, content_type)
            VALUES ($1, 0, 'Default', $2, $3);",
            round.id,
            data.content,
            data.content_type
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(result)
}

pub async fn admin_update(
    pool: &DbPool,
    round_id: i32,
    data: &RbRoundUpdateData,
) -> Result<Option<RbRoundAdminData>, RbInternalError> {
    let cover_is_set = data.cover.is_some();
    let cover = data.cover.clone().flatten();
    let puzzle_is_set = data.puzzle.is_some();
    let puzzle = data.puzzle.flatten();
    let slug_is_set = data.slug.is_some();
    let slug = data.slug.clone().flatten();
    let description_is_set = data.description.is_some();
    let description = data.description.clone().flatten();

    let result = sqlx::query_as!(
        RbRoundAdminData,
        "UPDATE rb_round r
        SET slug = CASE WHEN $2 THEN $3 ELSE r.slug END,
            sort = COALESCE($4, r.sort),
            title = COALESCE($5, r.title),
            description = CASE WHEN $6 THEN $7 ELSE r.description END,
            cover = CASE WHEN $8 THEN $9 ELSE r.cover END,
            puzzle = CASE
                WHEN $10 AND $11::INT IS NULL THEN NULL
                WHEN $10 THEN $11::INT
                ELSE r.puzzle
            END
        WHERE r.id = $1
            AND (
                NOT $10 OR $11::INT IS NULL OR EXISTS (
                    SELECT 1
                    FROM rb_puzzle p
                    WHERE p.id = $11::INT AND p.round_id = r.id
                )
            )
        RETURNING id, slug, sort, title, description, cover, game_id, puzzle;",
        round_id,
        slug_is_set,
        slug,
        data.sort,
        data.title,
        description_is_set,
        description,
        cover_is_set,
        cover,
        puzzle_is_set,
        puzzle
    )
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

pub async fn admin_delete(pool: &DbPool, round_id: i32) -> Result<bool, RbInternalError> {
    let result = sqlx::query!(
        "DELETE FROM rb_round
        WHERE id = $1;",
        round_id
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() > 0)
}

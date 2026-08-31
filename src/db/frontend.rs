use deadpool_redis::redis::AsyncCommands;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_repr::{Deserialize_repr, Serialize_repr};
use sqlx::{
    Decode, FromRow, PgConnection, PgPool, Postgres, Transaction, Type, postgres::PgValueRef,
};
use time::OffsetDateTime;

use crate::{KvPool, error::RbInternalError};

pub const ROUND_PAGE: &str = "round-page";
pub const PUZZLE_PAGE: &str = "puzzle-page";
#[derive(
    Debug,
    Clone,
    Copy,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    IntoPrimitive,
    TryFromPrimitive,
    Serialize_repr,
    Deserialize_repr,
)]
#[repr(i16)]
pub enum FrontendFeature {
    Locale = 0,
    Icons = 1,
    Ui = 2,
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum ThemeSyncMessage {
    Unknown = -1,
    SystemStatusUpdated = 1,
    GameNewAnnouncement = 101,
    GameReleaseUpdated = 102,
    GameFrontendUpdated = 103,
    TeamInfoUpdated = 201,
    TeamDisbanded = 202,
    TeamSelfKicked = 203,
    TeamSelfPromoted = 204,
    PuzzleSubmitted = 301,
    PuzzleHintUnlocked = 302,
    PuzzleBackendEvent = 303,
    TicketUpdated = 401,
    NotificationUpdated = 402,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeAllPermission {
    All,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ThemeSyncPermission {
    All(ThemeAllPermission),
    Messages(Vec<ThemeSyncMessage>),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemePermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<ThemeSyncPermission>,
}

impl Type<Postgres> for FrontendFeature {
    fn type_info() -> <Postgres as sqlx::Database>::TypeInfo {
        <i16 as Type<Postgres>>::type_info()
    }
}

impl<'r> Decode<'r, Postgres> for FrontendFeature {
    fn decode(value: PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::try_from(<i16 as Decode<Postgres>>::decode(value)?)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeManifest {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "apiVersion")]
    pub api_version: i32,
    pub package: ThemeManifestPackage,
    #[serde(default)]
    pub permissions: ThemePermissions,
    #[serde(default)]
    pub features: ThemeFeatures,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeFeatures {
    #[serde(default)]
    pub renderers: std::collections::BTreeMap<String, ThemeRendererManifest>,
    #[serde(default)]
    pub locale: Option<ThemeLocaleFeature>,
    #[serde(default)]
    pub icons: Option<ThemeIconsFeature>,
    #[serde(default)]
    pub ui: Option<ThemeUiFeature>,
}

impl ThemeFeatures {
    pub fn is_empty(&self) -> bool {
        self.renderers.is_empty()
            && self.locale.is_none()
            && self.icons.is_none()
            && self.ui.is_none()
    }

    pub fn contains(&self, feature: FrontendFeature) -> bool {
        match feature {
            FrontendFeature::Locale => self.locale.is_some(),
            FrontendFeature::Icons => self.icons.is_some(),
            FrontendFeature::Ui => self.ui.is_some(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeLocaleFeature {
    pub locales: std::collections::BTreeMap<String, ThemeLocaleEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ThemeLocaleEntry {
    Inline { messages: Value },
    Json { source: String },
    Module { source: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeIconsFeature {
    pub collections: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ThemeUiFeature {
    #[serde(default)]
    pub icons: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ThemeManifestPackage {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeRendererManifest {
    pub surface: String,
    #[serde(default)]
    pub layout: Option<String>,
    pub entry: String,
    #[serde(default)]
    pub styles: Vec<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FrontendPackage {
    pub id: i32,
    pub game_id: i32,
    pub asset_group_id: i32,
    pub name: String,
    pub version: String,
    pub manifest_path: String,
    pub manifest: Value,
    pub sha256: String,
    pub delete_pending: bool,
    pub ctime_at: OffsetDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FrontendRevision {
    pub id: i64,
    pub game_id: i32,
    pub revision: i64,
    pub status: String,
    pub created_by: Option<i32>,
    pub ctime_at: OffsetDateTime,
    pub published_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FrontendBinding {
    pub revision_id: i64,
    pub surface: String,
    pub scope_kind: String,
    pub scope_id: i32,
    pub package_id: Option<i32>,
    pub renderer_id: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct FrontendFeatureActivation {
    pub revision_id: i64,
    pub package_id: i32,
    pub feature: FrontendFeature,
}

#[derive(Debug, Clone, Deserialize, FromRow, Serialize)]
pub struct ResolvedBindingRow {
    pub revision: i64,
    pub package_id: Option<i32>,
    pub renderer_id: Option<String>,
    pub asset_group_id: Option<i32>,
    pub manifest_path: Option<String>,
    pub manifest: Option<Value>,
    pub object_key: Option<String>,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ResolvedFeatureRow {
    pub revision: i64,
    pub package_id: i32,
    pub feature: FrontendFeature,
    pub manifest_path: String,
    pub object_key: String,
    pub backend: String,
}

pub async fn list_packages(
    pool: &PgPool,
    game_id: i32,
) -> Result<Vec<FrontendPackage>, RbInternalError> {
    Ok(sqlx::query_as!(
        FrontendPackage,
        "SELECT id, game_id, asset_group_id, name, version, manifest_path, manifest, sha256, delete_pending, ctime_at
         FROM rb_frontend_package WHERE game_id = $1 ORDER BY ctime_at DESC, id DESC",
        game_id,
    )
    .fetch_all(pool)
    .await?)
}

pub async fn get_package(
    pool: &PgPool,
    game_id: i32,
    package_id: i32,
) -> Result<Option<FrontendPackage>, RbInternalError> {
    Ok(sqlx::query_as!(
        FrontendPackage,
        "SELECT id, game_id, asset_group_id, name, version, manifest_path, manifest, sha256, delete_pending, ctime_at
         FROM rb_frontend_package WHERE game_id = $1 AND id = $2",
        game_id,
        package_id,
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn get_package_by_name_for_update_conn(
    conn: &mut PgConnection,
    game_id: i32,
    name: &str,
) -> Result<Option<FrontendPackage>, RbInternalError> {
    Ok(sqlx::query_as!(
        FrontendPackage,
        "SELECT id, game_id, asset_group_id, name, version, manifest_path, manifest, sha256, delete_pending, ctime_at
         FROM rb_frontend_package WHERE game_id = $1 AND name = $2 AND NOT delete_pending FOR UPDATE",
        game_id,
        name,
    )
    .fetch_optional(&mut *conn)
    .await?)
}

pub struct NewPackage<'a> {
    pub game_id: i32,
    pub asset_group_id: i32,
    pub manifest_path: &'a str,
    pub manifest: &'a ThemeManifest,
    pub sha256: &'a str,
}

pub async fn create_package_conn(
    conn: &mut PgConnection,
    data: NewPackage<'_>,
) -> Result<FrontendPackage, RbInternalError> {
    let manifest = serde_json::to_value(data.manifest)?;
    Ok(sqlx::query_as!(
        FrontendPackage,
        "INSERT INTO rb_frontend_package (game_id, asset_group_id, name, version, manifest_path, manifest, sha256)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING id, game_id, asset_group_id, name, version, manifest_path, manifest, sha256, delete_pending, ctime_at",
        data.game_id,
        data.asset_group_id,
        data.manifest.package.name,
        data.manifest.package.version,
        data.manifest_path,
        manifest,
        data.sha256,
    )
    .fetch_one(conn)
    .await?)
}

pub async fn replace_package_conn(
    conn: &mut PgConnection,
    package_id: i32,
    data: NewPackage<'_>,
) -> Result<FrontendPackage, RbInternalError> {
    let manifest = serde_json::to_value(data.manifest)?;
    Ok(sqlx::query_as!(
        FrontendPackage,
        "UPDATE rb_frontend_package
         SET asset_group_id=$2, version=$3, manifest_path=$4, manifest=$5, sha256=$6, ctime_at=CURRENT_TIMESTAMP
         WHERE id=$1 AND game_id=$7
         RETURNING id, game_id, asset_group_id, name, version, manifest_path, manifest, sha256, delete_pending, ctime_at",
        package_id,
        data.asset_group_id,
        data.manifest.package.version,
        data.manifest_path,
        manifest,
        data.sha256,
        data.game_id,
    )
    .fetch_one(conn)
    .await?)
}

pub async fn asset_group_locked(
    pool: &PgPool,
    asset_group_id: i32,
) -> Result<bool, RbInternalError> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM rb_frontend_package WHERE asset_group_id=$1)",
    )
    .bind(asset_group_id)
    .fetch_one(pool)
    .await?)
}

async fn next_revision(
    tx: &mut Transaction<'_, Postgres>,
    game_id: i32,
) -> Result<i64, RbInternalError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(revision), 0) + 1 FROM rb_frontend_revision WHERE game_id=$1",
    )
    .bind(game_id)
    .fetch_one(&mut **tx)
    .await?)
}

pub async fn ensure_draft(
    pool: &PgPool,
    game_id: i32,
    user_id: i32,
) -> Result<FrontendRevision, RbInternalError> {
    if let Some(row) = sqlx::query_as::<_, FrontendRevision>(
        "SELECT id, game_id, revision, status, created_by, ctime_at, published_at FROM rb_frontend_revision WHERE game_id=$1 AND status='draft'",
    ).bind(game_id).fetch_optional(pool).await? { return Ok(row); }

    let mut tx = pool.begin().await?;
    let revision = next_revision(&mut tx, game_id).await?;
    let draft = sqlx::query_as::<_, FrontendRevision>(
        "INSERT INTO rb_frontend_revision (game_id, revision, status, created_by) VALUES ($1,$2,'draft',$3)
         RETURNING id, game_id, revision, status, created_by, ctime_at, published_at",
    ).bind(game_id).bind(revision).bind(user_id).fetch_one(&mut *tx).await?;
    if let Some(published_id) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM rb_frontend_revision WHERE game_id=$1 AND status='published'",
    )
    .bind(game_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        sqlx::query!(
            "INSERT INTO rb_frontend_binding (revision_id,surface,scope_kind,scope_id,package_id,renderer_id)
             SELECT $1,surface,scope_kind,scope_id,package_id,renderer_id FROM rb_frontend_binding WHERE revision_id=$2",
            draft.id,
            published_id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO rb_frontend_feature_activation (revision_id,package_id,feature)
             SELECT $1,package_id,feature FROM rb_frontend_feature_activation WHERE revision_id=$2",
            draft.id,
            published_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(draft)
}

pub async fn list_revisions(
    pool: &PgPool,
    game_id: i32,
) -> Result<Vec<FrontendRevision>, RbInternalError> {
    Ok(sqlx::query_as::<_, FrontendRevision>(
        "SELECT id, game_id, revision, status, created_by, ctime_at, published_at FROM rb_frontend_revision WHERE game_id=$1 ORDER BY revision DESC",
    ).bind(game_id).fetch_all(pool).await?)
}

pub async fn list_bindings(
    pool: &PgPool,
    revision_id: i64,
) -> Result<Vec<FrontendBinding>, RbInternalError> {
    Ok(sqlx::query_as!(
        FrontendBinding,
        "SELECT revision_id,surface,scope_kind,scope_id,package_id,renderer_id FROM rb_frontend_binding WHERE revision_id=$1 ORDER BY surface,scope_kind,scope_id",
        revision_id,
    )
    .fetch_all(pool)
    .await?)
}

pub async fn list_feature_activations(
    pool: &PgPool,
    revision_id: i64,
) -> Result<Vec<FrontendFeatureActivation>, RbInternalError> {
    Ok(sqlx::query_as!(
        FrontendFeatureActivation,
        r#"SELECT revision_id,package_id,feature AS "feature: FrontendFeature"
           FROM rb_frontend_feature_activation WHERE revision_id=$1 ORDER BY package_id,feature"#,
        revision_id,
    )
    .fetch_all(pool)
    .await?)
}

pub async fn set_feature_activation(
    pool: &PgPool,
    revision_id: i64,
    package_id: i32,
    feature: FrontendFeature,
    enabled: bool,
) -> Result<(), RbInternalError> {
    if enabled {
        sqlx::query!(
            "INSERT INTO rb_frontend_feature_activation (revision_id,package_id,feature) VALUES ($1,$2,$3)
             ON CONFLICT (revision_id,package_id,feature) DO NOTHING",
            revision_id,
            package_id,
            i16::from(feature),
        )
        .execute(pool)
        .await?;
    } else {
        sqlx::query!(
            "DELETE FROM rb_frontend_feature_activation WHERE revision_id=$1 AND package_id=$2 AND feature=$3",
            revision_id,
            package_id,
            i16::from(feature),
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn upsert_binding(
    pool: &PgPool,
    binding: &FrontendBinding,
) -> Result<(), RbInternalError> {
    sqlx::query!(
        "INSERT INTO rb_frontend_binding (revision_id,surface,scope_kind,scope_id,package_id,renderer_id)
         VALUES ($1,$2,$3,$4,$5,$6)
         ON CONFLICT (revision_id,surface,scope_kind,scope_id)
         DO UPDATE SET package_id=EXCLUDED.package_id,renderer_id=EXCLUDED.renderer_id",
        binding.revision_id,
        binding.surface,
        binding.scope_kind,
        binding.scope_id,
        binding.package_id,
        binding.renderer_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_binding(
    pool: &PgPool,
    revision_id: i64,
    surface: &str,
    scope_kind: &str,
    scope_id: i32,
) -> Result<bool, RbInternalError> {
    Ok(sqlx::query("DELETE FROM rb_frontend_binding WHERE revision_id=$1 AND surface=$2 AND scope_kind=$3 AND scope_id=$4")
        .bind(revision_id).bind(surface).bind(scope_kind).bind(scope_id).execute(pool).await?.rows_affected() > 0)
}

pub async fn publish(
    pool: &PgPool,
    game_id: i32,
    draft_id: i64,
    user_id: i32,
) -> Result<Option<(FrontendRevision, FrontendRevision)>, RbInternalError> {
    let mut tx = pool.begin().await?;
    let draft = sqlx::query_as::<_, FrontendRevision>(
        "SELECT id, game_id, revision, status, created_by, ctime_at, published_at FROM rb_frontend_revision WHERE id=$1 AND game_id=$2 AND status='draft' FOR UPDATE",
    ).bind(draft_id).bind(game_id).fetch_optional(&mut *tx).await?;
    let Some(_) = draft else {
        return Ok(None);
    };
    sqlx::query!(
        "UPDATE rb_frontend_binding b SET package_id=active.id
         FROM rb_frontend_revision r, rb_frontend_package old, rb_frontend_package active
         WHERE b.revision_id=r.id AND r.id=$1 AND b.package_id=old.id AND old.delete_pending
           AND active.game_id=old.game_id AND active.name=old.name AND NOT active.delete_pending",
        draft_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM rb_frontend_feature_activation old_feature
         USING rb_frontend_revision r, rb_frontend_package old, rb_frontend_package active
         WHERE old_feature.revision_id=r.id AND r.id=$1 AND old_feature.package_id=old.id AND old.delete_pending
           AND active.game_id=old.game_id AND active.name=old.name AND NOT active.delete_pending
           AND EXISTS(SELECT 1 FROM rb_frontend_feature_activation current_feature
                      WHERE current_feature.revision_id=old_feature.revision_id
                        AND current_feature.package_id=active.id AND current_feature.feature=old_feature.feature)",
        draft_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE rb_frontend_feature_activation f SET package_id=active.id
         FROM rb_frontend_revision r, rb_frontend_package old, rb_frontend_package active
         WHERE f.revision_id=r.id AND r.id=$1 AND f.package_id=old.id AND old.delete_pending
           AND active.game_id=old.game_id AND active.name=old.name AND NOT active.delete_pending",
        draft_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM rb_frontend_binding b USING rb_frontend_package p
         WHERE b.revision_id=$1 AND b.package_id=p.id AND p.delete_pending",
        draft_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM rb_frontend_feature_activation f USING rb_frontend_package p
         WHERE f.revision_id=$1 AND f.package_id=p.id AND p.delete_pending",
        draft_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM rb_frontend_revision WHERE game_id=$1 AND status='published'")
        .bind(game_id)
        .execute(&mut *tx)
        .await?;
    let published = sqlx::query_as::<_, FrontendRevision>(
        "UPDATE rb_frontend_revision SET status='published', published_at=NOW() WHERE id=$1
         RETURNING id, game_id, revision, status, created_by, ctime_at, published_at",
    )
    .bind(draft_id)
    .fetch_one(&mut *tx)
    .await?;
    let revision = next_revision(&mut tx, game_id).await?;
    let next = sqlx::query_as::<_, FrontendRevision>(
        "INSERT INTO rb_frontend_revision (game_id,revision,status,created_by) VALUES ($1,$2,'draft',$3)
         RETURNING id, game_id, revision, status, created_by, ctime_at, published_at",
    ).bind(game_id).bind(revision).bind(user_id).fetch_one(&mut *tx).await?;
    sqlx::query!(
        "INSERT INTO rb_frontend_binding (revision_id,surface,scope_kind,scope_id,package_id,renderer_id)
         SELECT $1,surface,scope_kind,scope_id,package_id,renderer_id FROM rb_frontend_binding WHERE revision_id=$2",
        next.id,
        published.id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO rb_frontend_feature_activation (revision_id,package_id,feature)
         SELECT $1,package_id,feature FROM rb_frontend_feature_activation WHERE revision_id=$2",
        next.id,
        published.id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((published, next)))
}

pub async fn resolve(
    pool: &PgPool,
    game_id: i32,
    surface: &str,
    round_id: Option<i32>,
    puzzle_id: Option<i32>,
    preview_revision: Option<i64>,
) -> Result<Option<ResolvedBindingRow>, RbInternalError> {
    Ok(sqlx::query_as!(
        ResolvedBindingRow,
        r#"SELECT r.revision, p.id AS "package_id?", b.renderer_id,
                p.asset_group_id AS "asset_group_id?",
                p.manifest_path AS "manifest_path?",
                p.manifest AS "manifest?",
                a.object_key AS "object_key?",
                a.backend AS "backend?"
         FROM rb_frontend_revision r JOIN rb_frontend_binding b ON b.revision_id=r.id
         LEFT JOIN rb_frontend_package source_package ON source_package.id=b.package_id
         LEFT JOIN rb_frontend_package p ON p.id=CASE
             WHEN r.status='draft' AND source_package.delete_pending THEN
                 (SELECT active.id FROM rb_frontend_package active
                  WHERE active.game_id=source_package.game_id AND active.name=source_package.name AND NOT active.delete_pending
                  LIMIT 1)
             ELSE source_package.id
         END
         LEFT JOIN rb_asset_group a ON a.id=p.asset_group_id
         WHERE r.game_id=$1 AND b.surface=$2
           AND (($5::BIGINT IS NULL AND r.status='published') OR r.id=$5)
           AND (b.package_id IS NULL OR p.id IS NOT NULL)
           AND ((b.scope_kind='puzzle' AND b.scope_id=$4) OR (b.scope_kind='round' AND b.scope_id=$3) OR (b.scope_kind='game' AND b.scope_id=0))
         ORDER BY CASE b.scope_kind WHEN 'puzzle' THEN 3 WHEN 'round' THEN 2 ELSE 1 END DESC LIMIT 1"#,
        game_id,
        surface,
        round_id.unwrap_or(0),
        puzzle_id.unwrap_or(0),
        preview_revision,
    )
        .fetch_optional(pool)
        .await?)
}

const RENDERER_CACHE_TTL_SECONDS: u64 = 24 * 60 * 60;

fn renderer_cache_revision(preview_revision: Option<i64>) -> String {
    preview_revision.map_or_else(|| "published".to_owned(), |id| id.to_string())
}

fn renderer_cache_key(
    game_id: i32,
    surface: &str,
    round_id: Option<i32>,
    puzzle_id: Option<i32>,
    preview_revision: Option<i64>,
) -> String {
    format!(
        "frontend:renderer:v2:game:{game_id}:revision:{}:surface:{surface}:round:{}:puzzle:{}",
        renderer_cache_revision(preview_revision),
        round_id.unwrap_or(0),
        puzzle_id.unwrap_or(0),
    )
}

pub async fn resolve_cached(
    pool: &PgPool,
    kv_pool: &KvPool,
    game_id: i32,
    surface: &str,
    round_id: Option<i32>,
    puzzle_id: Option<i32>,
    preview_revision: Option<i64>,
) -> Result<Option<ResolvedBindingRow>, RbInternalError> {
    let key = renderer_cache_key(game_id, surface, round_id, puzzle_id, preview_revision);
    let mut conn = kv_pool.get().await?;
    if let Some(cached) = conn.get::<_, Option<String>>(&key).await? {
        return Ok(serde_json::from_str(&cached)?);
    }

    let resolved = resolve(
        pool,
        game_id,
        surface,
        round_id,
        puzzle_id,
        preview_revision,
    )
    .await?;
    conn.set_ex::<_, _, ()>(
        key,
        serde_json::to_string(&resolved)?,
        RENDERER_CACHE_TTL_SECONDS,
    )
    .await?;
    Ok(resolved)
}

pub async fn invalidate_renderer_cache(
    kv_pool: &KvPool,
    game_id: i32,
    preview_revision: Option<i64>,
) -> Result<(), RbInternalError> {
    crate::db::cache::del_pattern(
        kv_pool,
        &format!(
            "frontend:renderer:v2:game:{game_id}:revision:{}:*",
            renderer_cache_revision(preview_revision),
        ),
    )
    .await
}

pub async fn resolve_features(
    pool: &PgPool,
    game_id: i32,
    preview_revision: Option<i64>,
) -> Result<Vec<ResolvedFeatureRow>, RbInternalError> {
    Ok(sqlx::query_as!(
        ResolvedFeatureRow,
        r#"SELECT DISTINCT r.revision, p.id AS "package_id!", f.feature AS "feature: FrontendFeature", p.manifest_path, a.object_key, a.backend
         FROM rb_frontend_revision r
         JOIN rb_frontend_feature_activation f ON f.revision_id=r.id
         JOIN rb_frontend_package source_package ON source_package.id=f.package_id AND source_package.game_id=r.game_id
         JOIN rb_frontend_package p ON p.id=CASE
             WHEN r.status='draft' AND source_package.delete_pending THEN
                 (SELECT active.id FROM rb_frontend_package active
                  WHERE active.game_id=source_package.game_id AND active.name=source_package.name AND NOT active.delete_pending
                  LIMIT 1)
             ELSE source_package.id
         END
         JOIN rb_asset_group a ON a.id=p.asset_group_id
         WHERE r.game_id=$1 AND (($2::BIGINT IS NULL AND r.status='published') OR r.id=$2)
         ORDER BY p.id,f.feature"#,
        game_id,
        preview_revision,
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::renderer_cache_key;

    #[test]
    fn renderer_cache_is_scoped_by_revision_surface_and_resource() {
        let published_round = renderer_cache_key(7, "round-page", Some(11), None, None);
        assert!(published_round.contains(":game:7:revision:published:"));
        assert_ne!(
            published_round,
            renderer_cache_key(7, "round-page", Some(12), None, None),
        );
        assert_ne!(
            published_round,
            renderer_cache_key(7, "puzzle-page", Some(11), Some(21), None),
        );
        assert_ne!(
            published_round,
            renderer_cache_key(7, "round-page", Some(11), None, Some(31)),
        );
    }
}

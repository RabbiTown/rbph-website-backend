use std::collections::{BTreeMap, HashMap, HashSet};

use actix_multipart::Multipart;
use actix_web::{HttpResponse, Result, web};
use futures_util::StreamExt;
use num_enum::IntoPrimitive;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_repr::Serialize_repr;

use crate::{
    AppState, db,
    error::{RbError, RbInternalError},
    extractor::auth::AuthUser,
    model::user::RbUserRole,
    module::storage::{AssetUploadFile, DATABASE_MAX_GROUP_FILES, LocalStorage, StoredAssetGroup},
};

const MANIFEST_PATH: &str = "rbph-theme.json";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_THEME_BYTES: usize = 64 * 1024 * 1024;

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendStateResult {
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendConfigResult {
    InvalidFormat = -1,
    MissingPackage = -2,
    InvalidBinding = -3,
    InvalidPackageManifest = -4,
    RendererRequired = -5,
    RendererUnavailable = -6,
    InvalidFeature = -7,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendPackageResult {
    GameNotFound = -1,
    ArchiveTooLarge = -2,
    ZipRequired = -3,
    InvalidArchive = -4,
    EmptyArchive = -5,
    InvalidManifest = -6,
    StorageFailed = -8,
    NameConflict = -9,
    PackageNotFound = -10,
    AssetNotFound = -11,
    NewerPackageExists = -12,
    NotPendingDeletion = -13,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendBindingResult {
    InvalidRevision = -1,
    InvalidSurface = -2,
    InvalidScope = -3,
    PackageNotFound = -4,
    PackagePendingDeletion = -5,
    InvalidPackageManifest = -6,
    RendererRequired = -7,
    RendererUnavailable = -8,
    UnexpectedRenderer = -9,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendFeatureResult {
    PackageNotFound = -1,
    PackagePendingDeletion = -2,
    InvalidPackageManifest = -3,
    InvalidRevision = -4,
    UnsupportedFeature = -5,
    Ok = 0,
}

#[repr(i32)]
#[derive(IntoPrimitive, Serialize_repr)]
enum FrontendPublishResult {
    RevisionNotFound = -1,
    Ok = 0,
}

async fn invalidate_draft_renderer_cache(app: &web::Data<AppState>, game_id: i32) -> Result<()> {
    let draft_id = sqlx::query_scalar!(
        "SELECT id FROM rb_frontend_revision WHERE game_id=$1 AND status='draft'",
        game_id,
    )
    .fetch_optional(&app.db)
    .await
    .map_err(RbInternalError::from)?;
    if let Some(draft_id) = draft_id {
        db::frontend::invalidate_renderer_cache(&app.kv, game_id, Some(draft_id)).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct GamePath {
    game_id: i32,
}
#[derive(Deserialize)]
struct PackagePath {
    game_id: i32,
    package_id: i32,
}
#[derive(Deserialize)]
struct RevisionPath {
    game_id: i32,
    revision_id: i64,
}

#[derive(Serialize)]
struct FrontendAdminState {
    code: FrontendStateResult,
    packages: Vec<FrontendAdminPackage>,
    revisions: Vec<db::frontend::FrontendRevision>,
    draft: db::frontend::FrontendRevision,
    bindings: Vec<db::frontend::FrontendBinding>,
    published_bindings: Vec<db::frontend::FrontendBinding>,
    feature_activations: Vec<db::frontend::FrontendFeatureActivation>,
    published_feature_activations: Vec<db::frontend::FrontendFeatureActivation>,
}

#[derive(Serialize)]
struct FrontendAdminPackage {
    #[serde(flatten)]
    package: db::frontend::FrontendPackage,
    manifest_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindingRequest {
    revision_id: i64,
    surface: String,
    scope_kind: String,
    scope_id: i32,
    package_id: Option<i32>,
    renderer_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeatureActivationRequest {
    revision_id: i64,
    package_id: i32,
    feature: db::frontend::FrontendFeature,
    enabled: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrontendConfig {
    format_version: i32,
    bindings: Vec<FrontendConfigBinding>,
    features: Vec<FrontendConfigFeatures>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrontendConfigBinding {
    surface: String,
    scope_kind: String,
    scope_id: i32,
    package_name: Option<String>,
    renderer_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrontendConfigFeatures {
    package_name: String,
    features: Vec<db::frontend::FrontendFeature>,
}

fn normalize_relative_path(path: &str) -> Option<&str> {
    let path = path.strip_prefix("./").unwrap_or(path);
    (!path.is_empty()
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".."))
    .then_some(path)
}

fn package_paths(manifest: &db::frontend::ThemeManifest) -> Option<HashSet<String>> {
    if manifest.kind != "rbph-theme"
        || manifest.api_version != 1
        || manifest.package.name.trim().is_empty()
        || manifest.package.version.trim().is_empty()
    {
        return None;
    }
    let mut paths = HashSet::new();
    if manifest.features.renderers.len() > 128
        || manifest.features.renderers.iter().any(|(id, renderer)| {
            id.is_empty()
                || id.len() > 120
                || !id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
                || !matches!(
                    renderer.surface.as_str(),
                    db::frontend::ROUND_PAGE | db::frontend::PUZZLE_PAGE
                )
        })
    {
        return None;
    }
    for renderer in manifest.features.renderers.values() {
        if renderer
            .layout
            .as_deref()
            .is_some_and(|layout| !matches!(layout, "game" | "game-full"))
            || normalize_relative_path(&renderer.entry).is_none()
            || renderer
                .styles
                .iter()
                .any(|p| normalize_relative_path(p).is_none())
        {
            return None;
        }
        paths.insert(normalize_relative_path(&renderer.entry)?.to_string());
        paths.extend(
            renderer
                .styles
                .iter()
                .filter_map(|path| normalize_relative_path(path).map(str::to_string)),
        );
    }
    if let Some(feature) = &manifest.features.locale {
        if feature.locales.is_empty()
            || feature.locales.len() > 32
            || feature.locales.iter().any(|(locale, entry)| {
                locale.is_empty()
                    || locale.len() > 32
                    || !locale.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                    })
                    || match entry {
                        db::frontend::ThemeLocaleEntry::Inline { messages } => {
                            !messages.is_object()
                        }
                        db::frontend::ThemeLocaleEntry::Json { source } => {
                            !source.to_ascii_lowercase().ends_with(".json")
                                || normalize_relative_path(source).is_none()
                        }
                        db::frontend::ThemeLocaleEntry::Module { source } => {
                            !source.to_ascii_lowercase().ends_with(".js")
                                || normalize_relative_path(source).is_none()
                        }
                    }
            })
        {
            return None;
        }
        paths.extend(feature.locales.values().filter_map(|entry| match entry {
            db::frontend::ThemeLocaleEntry::Inline { .. } => None,
            db::frontend::ThemeLocaleEntry::Json { source }
            | db::frontend::ThemeLocaleEntry::Module { source } => {
                normalize_relative_path(source).map(str::to_string)
            }
        }));
    }
    if let Some(feature) = &manifest.features.icons {
        if feature.collections.is_empty() || feature.collections.len() > 32 {
            return None;
        }
        for collection in &feature.collections {
            if let Some(path) = collection.as_str() {
                paths.insert(normalize_relative_path(path)?.to_string());
            } else if !valid_icon_collection(collection) {
                return None;
            }
        }
    }
    if let Some(feature) = &manifest.features.ui {
        if (feature.source.is_none() && feature.icons.is_empty())
            || (feature.source.is_some() && !feature.icons.is_empty())
            || feature
                .icons
                .iter()
                .any(|(position, icon)| position.is_empty() || icon.is_empty())
        {
            return None;
        }
        if let Some(source) = &feature.source {
            paths.insert(normalize_relative_path(source)?.to_string());
        }
    }
    Some(paths)
}

fn valid_icon_collection(collection: &Value) -> bool {
    let Some(prefix) = collection.get("prefix").and_then(Value::as_str) else {
        return false;
    };
    let Some(icons) = collection.get("icons").and_then(Value::as_object) else {
        return false;
    };
    !prefix.is_empty()
        && prefix.len() <= 64
        && prefix.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !icons.is_empty()
        && icons.iter().all(|(name, icon)| {
            !name.is_empty()
                && icon.is_object()
                && icon.get("body").and_then(Value::as_str).is_some()
        })
        && collection.get("aliases").is_none_or(|aliases| {
            aliases.as_object().is_some_and(|aliases| {
                aliases.iter().all(|(name, alias)| {
                    !name.is_empty()
                        && alias.is_object()
                        && alias.get("parent").and_then(Value::as_str).is_some()
                })
            })
        })
}

fn validate_theme_files(files: &[AssetUploadFile]) -> Option<db::frontend::ThemeManifest> {
    let file_paths = files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect::<HashSet<_>>();
    let manifest_file = files
        .iter()
        .find(|file| file.relative_path == MANIFEST_PATH)?;
    if manifest_file.bytes.len() as u64 > MAX_METADATA_BYTES {
        return None;
    }
    let manifest: db::frontend::ThemeManifest =
        serde_json::from_slice(&manifest_file.bytes).ok()?;
    let paths = package_paths(&manifest)?;
    if paths.iter().any(|path| !file_paths.contains(path.as_str())) {
        return None;
    }
    for path in manifest.features.locale.iter().flat_map(|feature| {
        feature.locales.values().filter_map(|entry| match entry {
            db::frontend::ThemeLocaleEntry::Json { source } => Some(source.as_str()),
            _ => None,
        })
    }) {
        let path = normalize_relative_path(path)?;
        let file = files.iter().find(|file| file.relative_path == path)?;
        if file.bytes.len() as u64 > MAX_METADATA_BYTES {
            return None;
        }
        let messages: Value = serde_json::from_slice(&file.bytes).ok()?;
        if !messages.is_object() {
            return None;
        }
    }
    for path in manifest
        .features
        .icons
        .iter()
        .flat_map(|feature| &feature.collections)
        .filter_map(Value::as_str)
    {
        let path = normalize_relative_path(path)?;
        let file = files.iter().find(|file| file.relative_path == path)?;
        if file.bytes.len() as u64 > MAX_METADATA_BYTES {
            return None;
        }
        let collection: Value = serde_json::from_slice(&file.bytes).ok()?;
        if !valid_icon_collection(&collection) {
            return None;
        }
    }
    if let Some(source) = manifest
        .features
        .ui
        .as_ref()
        .and_then(|feature| feature.source.as_deref())
    {
        let source = normalize_relative_path(source)?;
        let file = files.iter().find(|file| file.relative_path == source)?;
        if file.bytes.len() as u64 > MAX_METADATA_BYTES {
            return None;
        }
        let ui: Value = serde_json::from_slice(&file.bytes).ok()?;
        if !valid_ui(&ui) {
            return None;
        }
    }
    Some(manifest)
}

fn valid_ui(ui: &Value) -> bool {
    ui.get("icons")
        .and_then(Value::as_object)
        .is_some_and(|icons| {
            !icons.is_empty()
                && icons.iter().all(|(position, icon)| {
                    !position.is_empty() && icon.as_str().is_some_and(|icon| !icon.is_empty())
                })
        })
}

async fn get_state(
    path: web::Path<GamePath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    let mut packages = Vec::new();
    for package in db::frontend::list_packages(&app.db, path.game_id).await? {
        let manifest_url = match db::asset::admin_get_group(&app.db, package.asset_group_id).await?
        {
            Some(group) => app.storage.asset_public_url(
                &group.backend,
                &group.object_key,
                &package.manifest_path,
            ),
            None => None,
        };
        packages.push(FrontendAdminPackage {
            package,
            manifest_url,
        });
    }
    let revisions = db::frontend::list_revisions(&app.db, path.game_id).await?;
    let published = revisions
        .iter()
        .find(|revision| revision.status == "published");
    let published_bindings = match published {
        Some(revision) => db::frontend::list_bindings(&app.db, revision.id).await?,
        None => Vec::new(),
    };
    let published_feature_activations = match published {
        Some(revision) => db::frontend::list_feature_activations(&app.db, revision.id).await?,
        None => Vec::new(),
    };
    let bindings = db::frontend::list_bindings(&app.db, draft.id).await?;
    let feature_activations = db::frontend::list_feature_activations(&app.db, draft.id).await?;
    Ok(HttpResponse::Ok().json(FrontendAdminState {
        code: FrontendStateResult::Ok,
        packages,
        revisions,
        bindings,
        published_bindings,
        feature_activations,
        published_feature_activations,
        draft,
    }))
}

async fn get_config(
    path: web::Path<GamePath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    let packages = db::frontend::list_packages(&app.db, path.game_id).await?;
    let active_names = packages
        .iter()
        .filter(|package| !package.delete_pending)
        .map(|package| package.name.as_str())
        .collect::<HashSet<_>>();
    let names = packages
        .iter()
        .filter(|package| !package.delete_pending || active_names.contains(package.name.as_str()))
        .map(|package| (package.id, package.name.as_str()))
        .collect::<HashMap<_, _>>();
    let bindings = db::frontend::list_bindings(&app.db, draft.id)
        .await?
        .into_iter()
        .filter_map(|binding| {
            let package_name = match binding.package_id {
                Some(package_id) => Some(names.get(&package_id)?.to_string()),
                None => None,
            };
            Some(FrontendConfigBinding {
                surface: binding.surface,
                scope_kind: binding.scope_kind,
                scope_id: binding.scope_id,
                package_name,
                renderer_id: binding.renderer_id,
            })
        })
        .collect();
    let mut features = BTreeMap::<String, Vec<db::frontend::FrontendFeature>>::new();
    for activation in db::frontend::list_feature_activations(&app.db, draft.id).await? {
        if let Some(name) = names.get(&activation.package_id) {
            features
                .entry((*name).to_string())
                .or_default()
                .push(activation.feature);
        }
    }
    Ok(HttpResponse::Ok().json(FrontendConfig {
        format_version: 1,
        bindings,
        features: features
            .into_iter()
            .map(|(package_name, features)| FrontendConfigFeatures {
                package_name,
                features,
            })
            .collect(),
    }))
}

async fn put_config(
    path: web::Path<GamePath>,
    body: web::Json<Value>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let body: FrontendConfig = serde_json::from_value(body.into_inner())
        .map_err(|_| RbError::bad_req(FrontendConfigResult::InvalidFormat.into()))?;
    if body.format_version != 1 {
        return RbError::bad_req(FrontendConfigResult::InvalidFormat.into()).http_err();
    }
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    let packages = db::frontend::list_packages(&app.db, path.game_id).await?;
    let active_packages = packages
        .iter()
        .filter(|package| !package.delete_pending)
        .map(|package| (package.name.as_str(), package))
        .collect::<HashMap<_, _>>();
    let referenced_names = body
        .bindings
        .iter()
        .filter_map(|binding| binding.package_name.as_deref())
        .chain(
            body.features
                .iter()
                .map(|features| features.package_name.as_str()),
        )
        .collect::<HashSet<_>>();
    let missing = referenced_names
        .iter()
        .filter(|name| !active_packages.contains_key(**name))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return RbError::conflict(FrontendConfigResult::MissingPackage.into())
            .msg(format!("missing theme packages: {}", missing.join(", ")))
            .http_err();
    }

    let mut binding_keys = HashSet::new();
    let mut resolved_bindings = Vec::with_capacity(body.bindings.len());
    for binding in &body.bindings {
        if !matches!(
            binding.surface.as_str(),
            db::frontend::ROUND_PAGE | db::frontend::PUZZLE_PAGE
        ) || (binding.surface == db::frontend::ROUND_PAGE && binding.scope_kind == "puzzle")
            || !scope_valid(&app.db, path.game_id, &binding.scope_kind, binding.scope_id).await?
            || !binding_keys.insert((
                binding.surface.as_str(),
                binding.scope_kind.as_str(),
                binding.scope_id,
            ))
        {
            return RbError::bad_req(FrontendConfigResult::InvalidBinding.into()).http_err();
        }
        let package_id = if let Some(name) = binding.package_name.as_deref() {
            let package = active_packages[name];
            let manifest: db::frontend::ThemeManifest =
                serde_json::from_value(package.manifest.clone()).map_err(|_| {
                    RbError::bad_req(FrontendConfigResult::InvalidPackageManifest.into())
                })?;
            let Some(renderer_id) = binding.renderer_id.as_deref() else {
                return RbError::bad_req(FrontendConfigResult::RendererRequired.into()).http_err();
            };
            if manifest
                .features
                .renderers
                .get(renderer_id)
                .is_none_or(|renderer| renderer.surface != binding.surface)
            {
                return RbError::bad_req(FrontendConfigResult::RendererUnavailable.into())
                    .http_err();
            }
            Some(package.id)
        } else {
            if binding.renderer_id.is_some() {
                return RbError::bad_req(FrontendConfigResult::InvalidBinding.into()).http_err();
            }
            None
        };
        resolved_bindings.push((binding, package_id));
    }

    let mut feature_keys = HashSet::new();
    let mut resolved_features = Vec::new();
    for selection in &body.features {
        let package = active_packages[selection.package_name.as_str()];
        let manifest: db::frontend::ThemeManifest =
            serde_json::from_value(package.manifest.clone()).map_err(|_| {
                RbError::bad_req(FrontendConfigResult::InvalidPackageManifest.into())
            })?;
        for feature in &selection.features {
            if !manifest.features.contains(*feature) || !feature_keys.insert((package.id, *feature))
            {
                return RbError::bad_req(FrontendConfigResult::InvalidFeature.into()).http_err();
            }
            resolved_features.push((package.id, *feature));
        }
    }

    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    sqlx::query!(
        "DELETE FROM rb_frontend_binding WHERE revision_id=$1",
        draft.id
    )
    .execute(&mut *tx)
    .await
    .map_err(RbInternalError::from)?;
    sqlx::query!(
        "DELETE FROM rb_frontend_feature_activation WHERE revision_id=$1",
        draft.id
    )
    .execute(&mut *tx)
    .await
    .map_err(RbInternalError::from)?;
    for (binding, package_id) in resolved_bindings {
        sqlx::query!(
            "INSERT INTO rb_frontend_binding (revision_id,surface,scope_kind,scope_id,package_id,renderer_id) VALUES ($1,$2,$3,$4,$5,$6)",
            draft.id,
            binding.surface,
            binding.scope_kind,
            binding.scope_id,
            package_id,
            binding.renderer_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(RbInternalError::from)?;
    }
    for (package_id, feature) in resolved_features {
        sqlx::query!(
            "INSERT INTO rb_frontend_feature_activation (revision_id,package_id,feature) VALUES ($1,$2,$3)",
            draft.id,
            package_id,
            i16::from(feature),
        )
        .execute(&mut *tx)
        .await
        .map_err(RbInternalError::from)?;
    }
    tx.commit().await.map_err(RbInternalError::from)?;
    db::frontend::invalidate_renderer_cache(&app.kv, path.game_id, Some(draft.id)).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"code":FrontendConfigResult::Ok})))
}

async fn upload_package(
    path: web::Path<GamePath>,
    mut payload: Multipart,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    if !db::game::exists(&app.db, path.game_id, RbUserRole::Admin).await? {
        return RbError::not_found()
            .code(FrontendPackageResult::GameNotFound.into())
            .http_err();
    }
    let mut upload = None;
    while let Some(field) = payload.next().await {
        let mut field =
            field.map_err(|_| RbError::bad_req(FrontendPackageResult::InvalidArchive.into()))?;
        if field.name() != Some("file") || upload.is_some() {
            while let Some(chunk) = field.next().await {
                let _ = chunk
                    .map_err(|_| RbError::bad_req(FrontendPackageResult::InvalidArchive.into()))?;
            }
            continue;
        }
        let name = field
            .content_disposition()
            .and_then(|value| value.get_filename())
            .unwrap_or("theme.zip")
            .to_string();
        let mut bytes = Vec::new();
        while let Some(chunk) = field.next().await {
            let chunk = chunk
                .map_err(|_| RbError::bad_req(FrontendPackageResult::InvalidArchive.into()))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_THEME_BYTES {
                return RbError::bad_req(FrontendPackageResult::ArchiveTooLarge.into()).http_err();
            }
            bytes.extend_from_slice(&chunk);
        }
        upload = Some((name, bytes));
    }
    let Some((file_name, archive)) =
        upload.filter(|(name, _)| name.to_ascii_lowercase().ends_with(".zip"))
    else {
        return RbError::bad_req(FrontendPackageResult::ZipRequired.into()).http_err();
    };
    let files = LocalStorage::unpack_zip_files_limited(
        &archive,
        MAX_THEME_BYTES as u64,
        MAX_THEME_BYTES as u64,
        DATABASE_MAX_GROUP_FILES,
    )
    .map_err(|_| RbError::bad_req(FrontendPackageResult::InvalidArchive.into()))?;
    if files.is_empty() {
        return RbError::bad_req(FrontendPackageResult::EmptyArchive.into()).http_err();
    }
    let Some(manifest) = validate_theme_files(&files) else {
        return RbError::bad_req(FrontendPackageResult::InvalidManifest.into()).http_err();
    };
    let backend = app
        .storage
        .available_backends()
        .into_iter()
        .filter(|backend| backend.public_read)
        .min_by_key(|backend| !backend.recommended)
        .ok_or_else(|| RbError::internal("no public asset backend is configured"))?;
    let object_key = format!("group-{}", uuid::Uuid::new_v4());
    let StoredAssetGroup {
        size,
        sha256,
        files: stored_files,
    } = app
        .storage
        .store_group_files(&backend.id, &object_key, &files)
        .await
        .map_err(|_| RbError::unprocessable(FrontendPackageResult::StorageFailed.into()))?;

    let mut tx = match app.db.begin().await {
        Ok(tx) => tx,
        Err(error) => {
            let paths = stored_files
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<Vec<_>>();
            let _ = app
                .storage
                .delete_files(&backend.id, &object_key, &paths)
                .await;
            return Err(RbInternalError::from(error).into());
        }
    };
    let result = async {
        let previous = db::frontend::get_package_by_name_for_update_conn(
            &mut tx,
            path.game_id,
            &manifest.package.name,
        )
        .await?;
        let published_referenced = if let Some(package) = &previous {
            sqlx::query_scalar!(
                r#"SELECT (EXISTS(
                        SELECT 1 FROM rb_frontend_binding b JOIN rb_frontend_revision r ON r.id=b.revision_id
                        WHERE r.game_id=$1 AND r.status='published' AND b.package_id=$2
                    ) OR EXISTS(
                        SELECT 1 FROM rb_frontend_feature_activation f JOIN rb_frontend_revision r ON r.id=f.revision_id
                        WHERE r.game_id=$1 AND r.status='published' AND f.package_id=$2
                    )) AS "referenced!""#,
                path.game_id,
                package.id,
            )
            .fetch_one(&mut *tx)
            .await?
        } else {
            false
        };
        let replaced_assets = if let Some(package) = &previous
            && !published_referenced
        {
            let group = db::asset::admin_get_group_conn(&mut tx, package.asset_group_id)
                .await?
                .ok_or_else(|| {
                    RbInternalError::Other("theme package asset group is missing".to_string())
                })?;
            let files = db::asset::list_files_conn(&mut tx, group.id).await?;
            Some((group, files))
        } else {
            None
        };
        if let Some(package) = &previous
            && published_referenced
        {
            sqlx::query!(
                "UPDATE rb_frontend_package SET delete_pending=TRUE WHERE id=$1",
                package.id,
            )
            .execute(&mut *tx)
            .await?;
        }
        let group = db::asset::create_group_conn(
            &mut tx,
            db::asset::CreateAssetGroupData {
                game_id: path.game_id,
                puzzle_id: None,
                round_id: None,
                backend: &backend.id,
                object_key: &object_key,
                original_name: &file_name,
                mime_type: "application/zip",
                size: size as i64,
                sha256: &sha256,
            },
        )
        .await?;
        for file in &stored_files {
            db::asset::create_file_conn(
                &mut tx,
                group.id,
                &file.relative_path,
                &file.mime_type,
                file.size as i64,
                &file.sha256,
            )
            .await?;
        }
        let data = db::frontend::NewPackage {
            game_id: path.game_id,
            asset_group_id: group.id,
            manifest_path: MANIFEST_PATH,
            manifest: &manifest,
            sha256: &sha256,
        };
        let package = if let Some(previous) = &previous
            && !published_referenced
        {
            db::frontend::replace_package_conn(&mut tx, previous.id, data).await?
        } else {
            db::frontend::create_package_conn(&mut tx, data).await?
        };
        let round_renderers = manifest
            .features
            .renderers
            .iter()
            .filter(|(_, renderer)| renderer.surface == db::frontend::ROUND_PAGE)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let puzzle_renderers = manifest
            .features
            .renderers
            .iter()
            .filter(|(_, renderer)| renderer.surface == db::frontend::PUZZLE_PAGE)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let features = [
            db::frontend::FrontendFeature::Locale,
            db::frontend::FrontendFeature::Icons,
            db::frontend::FrontendFeature::Ui,
        ]
        .into_iter()
        .filter(|feature| manifest.features.contains(*feature))
        .map(i16::from)
        .collect::<Vec<_>>();
        sqlx::query!(
            "DELETE FROM rb_frontend_binding b USING rb_frontend_revision r, rb_frontend_package p
             WHERE b.revision_id=r.id AND r.game_id=$1 AND r.status='draft'
               AND b.package_id=p.id AND p.game_id=$1 AND p.name=$2
               AND NOT ((b.surface='round-page' AND b.renderer_id=ANY($3))
                     OR (b.surface='puzzle-page' AND b.renderer_id=ANY($4)))",
            path.game_id,
            manifest.package.name,
            &round_renderers,
            &puzzle_renderers,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE rb_frontend_binding b SET package_id=$3
             FROM rb_frontend_revision r, rb_frontend_package p
             WHERE b.revision_id=r.id AND r.game_id=$1 AND r.status='draft'
               AND b.package_id=p.id AND p.game_id=$1 AND p.name=$2 AND p.id<>$3",
            path.game_id,
            manifest.package.name,
            package.id,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM rb_frontend_feature_activation f USING rb_frontend_revision r, rb_frontend_package p
             WHERE f.revision_id=r.id AND r.game_id=$1 AND r.status='draft'
               AND f.package_id=p.id AND p.game_id=$1 AND p.name=$2
               AND NOT (f.feature=ANY($3))",
            path.game_id,
            manifest.package.name,
            &features,
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE rb_frontend_feature_activation f SET package_id=$3
             FROM rb_frontend_revision r, rb_frontend_package p
             WHERE f.revision_id=r.id AND r.game_id=$1 AND r.status='draft'
               AND f.package_id=p.id AND p.game_id=$1 AND p.name=$2 AND p.id<>$3",
            path.game_id,
            manifest.package.name,
            package.id,
        )
        .execute(&mut *tx)
        .await?;
        if let Some((old_group, _)) = &replaced_assets {
            db::asset::admin_delete_group(&mut *tx, old_group.id).await?;
        }
        Ok::<_, RbInternalError>((package, replaced_assets))
    }
    .await;
    let (package, replaced_assets) = match result {
        Ok(result) => {
            tx.commit().await.map_err(RbInternalError::from)?;
            result
        }
        Err(RbInternalError::Sql(sqlx::Error::Database(db)))
            if db.code().as_deref() == Some("23505") =>
        {
            let paths = stored_files
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<Vec<_>>();
            let _ = app
                .storage
                .delete_files(&backend.id, &object_key, &paths)
                .await;
            return RbError::conflict(FrontendPackageResult::NameConflict.into()).http_err();
        }
        Err(error) => {
            let paths = stored_files
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<Vec<_>>();
            let _ = app
                .storage
                .delete_files(&backend.id, &object_key, &paths)
                .await;
            return Err(error.into());
        }
    };
    let replaced = replaced_assets.is_some();
    if let Some((group, files)) = replaced_assets {
        let paths = files
            .into_iter()
            .map(|file| file.relative_path)
            .collect::<Vec<_>>();
        if let Err(error) = app
            .storage
            .delete_files(&group.backend, &group.object_key, &paths)
            .await
        {
            log::warn!("failed to remove replaced theme package files: {error}");
        }
    }
    if replaced {
        db::frontend::invalidate_renderer_cache(&app.kv, path.game_id, None).await?;
        let published_revision = sqlx::query_scalar!(
            "SELECT revision
             FROM rb_frontend_revision r
             WHERE r.game_id=$1 AND r.status='published'
               AND (EXISTS(SELECT 1 FROM rb_frontend_binding b WHERE b.revision_id=r.id AND b.package_id=$2)
                 OR EXISTS(SELECT 1 FROM rb_frontend_feature_activation f WHERE f.revision_id=r.id AND f.package_id=$2))",
            path.game_id,
            package.id,
        )
        .fetch_optional(&app.db)
        .await
        .map_err(RbInternalError::from)?;
        if let Some(revision) = published_revision {
            app.sync_hub
                .notify_game_frontend_updated(path.game_id, revision)
                .await;
        }
    }
    invalidate_draft_renderer_cache(&app, path.game_id).await?;
    Ok(HttpResponse::Ok()
        .json(serde_json::json!({"code":FrontendPackageResult::Ok,"package":package})))
}

async fn delete_package(
    path: web::Path<PackagePath>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let package = db::frontend::get_package(&app.db, path.game_id, path.package_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(FrontendPackageResult::PackageNotFound.into()))?;
    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let published_referenced = sqlx::query_scalar!(
        r#"SELECT (EXISTS(
                SELECT 1 FROM rb_frontend_binding b JOIN rb_frontend_revision r ON r.id=b.revision_id
                WHERE r.game_id=$1 AND r.status='published' AND b.package_id=$2
            ) OR EXISTS(
                SELECT 1 FROM rb_frontend_feature_activation f JOIN rb_frontend_revision r ON r.id=f.revision_id
                WHERE r.game_id=$1 AND r.status='published' AND f.package_id=$2
            )) AS "referenced!""#,
        path.game_id,
        path.package_id,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(RbInternalError::from)?;
    if published_referenced {
        sqlx::query!(
            "UPDATE rb_frontend_package SET delete_pending=TRUE WHERE id=$1 AND game_id=$2",
            path.package_id,
            path.game_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(RbInternalError::from)?;
    } else {
        sqlx::query!(
            "DELETE FROM rb_frontend_binding WHERE package_id=$1",
            path.package_id
        )
        .execute(&mut *tx)
        .await
        .map_err(RbInternalError::from)?;
        sqlx::query!(
            "DELETE FROM rb_frontend_feature_activation WHERE package_id=$1",
            path.package_id
        )
        .execute(&mut *tx)
        .await
        .map_err(RbInternalError::from)?;
    }
    tx.commit().await.map_err(RbInternalError::from)?;
    invalidate_draft_renderer_cache(&app, path.game_id).await?;
    if published_referenced {
        return Ok(HttpResponse::Ok()
            .json(serde_json::json!({"code":FrontendPackageResult::Ok,"deleted":false,"deletePending":true})));
    }
    let deleted = permanently_delete_package(&app, package).await?;
    Ok(HttpResponse::Ok()
        .json(serde_json::json!({"code":FrontendPackageResult::Ok,"deleted":deleted,"deletePending":false})))
}

async fn permanently_delete_package(
    app: &web::Data<AppState>,
    package: db::frontend::FrontendPackage,
) -> Result<bool> {
    let group = db::asset::admin_get_group(&app.db, package.asset_group_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(FrontendPackageResult::AssetNotFound.into()))?;
    let files = db::asset::list_files(&app.db, group.id).await?;
    let mut tx = app.db.begin().await.map_err(RbInternalError::from)?;
    let deleted = sqlx::query_scalar!(
        "DELETE FROM rb_frontend_package p WHERE p.id=$1 AND p.game_id=$2
         AND NOT EXISTS(SELECT 1 FROM rb_frontend_binding WHERE package_id=p.id)
         AND NOT EXISTS(SELECT 1 FROM rb_frontend_feature_activation WHERE package_id=p.id)
         RETURNING p.id",
        package.id,
        package.game_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(RbInternalError::from)?
    .is_some();
    if !deleted {
        return Ok(false);
    }
    db::asset::admin_delete_group(&mut *tx, group.id).await?;
    tx.commit().await.map_err(RbInternalError::from)?;
    let paths = files
        .into_iter()
        .map(|file| file.relative_path)
        .collect::<Vec<_>>();
    app.storage
        .delete_files(&group.backend, &group.object_key, &paths)
        .await
        .map_err(|_| RbError::internal("failed to remove theme package files"))?;
    Ok(true)
}

async fn restore_package(
    path: web::Path<PackagePath>,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let package = db::frontend::get_package(&app.db, path.game_id, path.package_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(FrontendPackageResult::PackageNotFound.into()))?;
    if !package.delete_pending {
        return RbError::conflict(FrontendPackageResult::NotPendingDeletion.into()).http_err();
    }
    let restored = sqlx::query_scalar!(
        "UPDATE rb_frontend_package p SET delete_pending=FALSE
         WHERE p.id=$1 AND p.game_id=$2 AND p.delete_pending
           AND NOT EXISTS(SELECT 1 FROM rb_frontend_package active
                          WHERE active.game_id=p.game_id AND active.name=p.name AND active.id<>p.id AND NOT active.delete_pending)
         RETURNING p.id",
        path.package_id,
        path.game_id,
    )
    .fetch_optional(&app.db)
    .await
    .map_err(RbInternalError::from)?;
    if restored.is_none() {
        return RbError::conflict(FrontendPackageResult::NewerPackageExists.into()).http_err();
    }
    invalidate_draft_renderer_cache(&app, path.game_id).await?;
    Ok(HttpResponse::Ok()
        .json(serde_json::json!({"code":FrontendPackageResult::Ok,"restored":true})))
}

async fn cleanup_pending_packages(app: &web::Data<AppState>, game_id: i32) -> Result<()> {
    let packages = db::frontend::list_packages(&app.db, game_id).await?;
    for package in packages
        .into_iter()
        .filter(|package| package.delete_pending)
    {
        permanently_delete_package(app, package).await?;
    }
    Ok(())
}

async fn scope_valid(
    pool: &sqlx::PgPool,
    game_id: i32,
    kind: &str,
    id: i32,
) -> Result<bool, RbInternalError> {
    match kind {
        "game" => Ok(id == 0),
        "round" => Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM rb_round WHERE id=$1 AND game_id=$2)",
        )
        .bind(id)
        .bind(game_id)
        .fetch_one(pool)
        .await?),
        "puzzle" => Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM rb_puzzle WHERE id=$1 AND game_id=$2)",
        )
        .bind(id)
        .bind(game_id)
        .fetch_one(pool)
        .await?),
        _ => Ok(false),
    }
}

async fn save_binding(
    path: web::Path<GamePath>,
    body: web::Json<BindingRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    if draft.id != body.revision_id {
        return RbError::conflict(FrontendBindingResult::InvalidRevision.into()).http_err();
    }
    if !matches!(
        body.surface.as_str(),
        db::frontend::ROUND_PAGE | db::frontend::PUZZLE_PAGE
    ) || (body.surface == db::frontend::ROUND_PAGE && body.scope_kind == "puzzle")
    {
        return RbError::bad_req(FrontendBindingResult::InvalidSurface.into()).http_err();
    }
    if !scope_valid(&app.db, path.game_id, &body.scope_kind, body.scope_id).await? {
        return RbError::bad_req(FrontendBindingResult::InvalidScope.into()).http_err();
    }
    if let Some(package_id) = body.package_id {
        let package = db::frontend::get_package(&app.db, path.game_id, package_id)
            .await?
            .ok_or_else(|| {
                RbError::not_found().code(FrontendBindingResult::PackageNotFound.into())
            })?;
        if package.delete_pending {
            return RbError::conflict(FrontendBindingResult::PackagePendingDeletion.into())
                .http_err();
        }
        let manifest: db::frontend::ThemeManifest =
            serde_json::from_value(package.manifest.clone()).map_err(|_| {
                RbError::bad_req(FrontendBindingResult::InvalidPackageManifest.into())
            })?;
        let Some(renderer_id) = body.renderer_id.as_deref() else {
            return RbError::bad_req(FrontendBindingResult::RendererRequired.into()).http_err();
        };
        if manifest
            .features
            .renderers
            .get(renderer_id)
            .is_none_or(|renderer| renderer.surface != body.surface)
        {
            return RbError::bad_req(FrontendBindingResult::RendererUnavailable.into()).http_err();
        }
    } else if body.renderer_id.is_some() {
        return RbError::bad_req(FrontendBindingResult::UnexpectedRenderer.into()).http_err();
    }
    db::frontend::upsert_binding(
        &app.db,
        &db::frontend::FrontendBinding {
            revision_id: body.revision_id,
            surface: body.surface.clone(),
            scope_kind: body.scope_kind.clone(),
            scope_id: body.scope_id,
            package_id: body.package_id,
            renderer_id: body.renderer_id.clone(),
        },
    )
    .await?;
    db::frontend::invalidate_renderer_cache(&app.kv, path.game_id, Some(body.revision_id)).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"code":FrontendBindingResult::Ok})))
}

async fn remove_binding(
    path: web::Path<GamePath>,
    query: web::Query<BindingRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    if draft.id != query.revision_id {
        return RbError::conflict(FrontendBindingResult::InvalidRevision.into()).http_err();
    }
    let deleted = db::frontend::delete_binding(
        &app.db,
        query.revision_id,
        &query.surface,
        &query.scope_kind,
        query.scope_id,
    )
    .await?;
    db::frontend::invalidate_renderer_cache(&app.kv, path.game_id, Some(query.revision_id)).await?;
    Ok(HttpResponse::Ok()
        .json(serde_json::json!({"code":FrontendBindingResult::Ok,"deleted":deleted})))
}

async fn save_feature_activation(
    path: web::Path<GamePath>,
    body: web::Json<FeatureActivationRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let draft = db::frontend::ensure_draft(&app.db, path.game_id, user.uid).await?;
    let package = db::frontend::get_package(&app.db, path.game_id, body.package_id)
        .await?
        .ok_or_else(|| RbError::not_found().code(FrontendFeatureResult::PackageNotFound.into()))?;
    if package.delete_pending {
        return RbError::conflict(FrontendFeatureResult::PackagePendingDeletion.into()).http_err();
    }
    let manifest: db::frontend::ThemeManifest = serde_json::from_value(package.manifest)
        .map_err(|_| RbError::bad_req(FrontendFeatureResult::InvalidPackageManifest.into()))?;
    if draft.id != body.revision_id {
        return RbError::conflict(FrontendFeatureResult::InvalidRevision.into()).http_err();
    }
    if !manifest.features.contains(body.feature) {
        return RbError::bad_req(FrontendFeatureResult::UnsupportedFeature.into()).http_err();
    }
    sqlx::query!(
        "DELETE FROM rb_frontend_feature_activation f USING rb_frontend_package pending, rb_frontend_package active
         WHERE f.revision_id=$1 AND f.package_id=pending.id AND pending.delete_pending
           AND active.id=$2 AND active.game_id=pending.game_id AND active.name=pending.name",
        draft.id,
        body.package_id,
    )
    .execute(&app.db)
    .await
    .map_err(RbInternalError::from)?;
    db::frontend::set_feature_activation(
        &app.db,
        draft.id,
        body.package_id,
        body.feature,
        body.enabled,
    )
    .await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({"code":FrontendFeatureResult::Ok})))
}

async fn publish(
    path: web::Path<RevisionPath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let result = db::frontend::publish(&app.db, path.game_id, path.revision_id, user.uid)
        .await?
        .ok_or_else(|| RbError::not_found().code(FrontendPublishResult::RevisionNotFound.into()))?;
    cleanup_pending_packages(&app, path.game_id).await?;
    db::frontend::invalidate_renderer_cache(&app.kv, path.game_id, None).await?;
    app.sync_hub
        .notify_game_frontend_updated(path.game_id, result.0.revision)
        .await;
    Ok(HttpResponse::Ok().json(
        serde_json::json!({"code":FrontendPublishResult::Ok,"published":result.0,"draft":result.1}),
    ))
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/{game_id}/frontend")
            .route("", web::get().to(get_state))
            .route("/config", web::get().to(get_config))
            .route("/config", web::put().to(put_config))
            .route("/packages", web::post().to(upload_package))
            .route("/packages/{package_id}", web::delete().to(delete_package))
            .route(
                "/packages/{package_id}/restore",
                web::post().to(restore_package),
            )
            .route("/bindings", web::put().to(save_binding))
            .route("/bindings", web::delete().to(remove_binding))
            .route("/features", web::put().to(save_feature_activation))
            .route("/revisions/{revision_id}/publish", web::post().to(publish)),
    );
}

#[cfg(test)]
mod tests {
    use super::{package_paths, validate_theme_files};
    use crate::db::frontend::ThemeManifest;
    use crate::module::storage::AssetUploadFile;
    use serde_json::json;

    #[test]
    fn manifest_paths_accept_dot_prefix_and_are_normalized() {
        let manifest: ThemeManifest = serde_json::from_value(json!({
            "type":"rbph-theme", "apiVersion":1,
            "package":{"name":"test","version":"1.0.0"},
            "features":{"renderers":{"round-main":{"surface":"round-page","entry":"./assets/theme.js","styles":["./assets/theme.css"]}}}
        }))
        .unwrap();
        let paths = package_paths(&manifest).unwrap();
        assert!(paths.contains("assets/theme.js"));
        assert!(paths.contains("assets/theme.css"));
    }

    #[test]
    fn manifest_accepts_multiple_renderers_for_one_surface() {
        let manifest: ThemeManifest = serde_json::from_value(json!({
            "type":"rbph-theme", "apiVersion":1,
            "package":{"name":"test","version":"1.0.0"},
            "features":{"renderers":{
                "round-map":{"surface":"round-page","entry":"assets/map.js"},
                "round-tree":{"surface":"round-page","entry":"assets/tree.js"}
            }}
        }))
        .unwrap();
        assert!(package_paths(&manifest).is_some());
    }

    #[test]
    fn manifest_rejects_unknown_layout() {
        let manifest: ThemeManifest = serde_json::from_value(json!({
            "type":"rbph-theme", "apiVersion":1,
            "package":{"name":"test","version":"1.0.0"},
            "features":{"renderers":{"round-main":{"surface":"round-page","layout":"fullscreen","entry":"assets/theme.js"}}}
        }))
        .unwrap();
        assert!(package_paths(&manifest).is_none());
    }

    #[test]
    fn manifest_accepts_feature_only_package() {
        let manifest: ThemeManifest = serde_json::from_value(json!({
            "type":"rbph-theme", "apiVersion":1,
            "package":{"name":"terminology","version":"1.0.0"},
            "features":{"locale":{"locales":{"zh-CN":{"type":"json","source":"features/zh-CN.json"}}}}
        }))
        .unwrap();
        assert_eq!(
            package_paths(&manifest).unwrap(),
            std::collections::HashSet::from(["features/zh-CN.json".to_string()])
        );
    }

    #[test]
    fn validates_locale_file_and_inline_features() {
        let file = |relative_path: &str, value: serde_json::Value| AssetUploadFile {
            relative_path: relative_path.to_string(),
            bytes: serde_json::to_vec(&value).unwrap(),
            mime_type: "application/json".to_string(),
        };
        let files = vec![
            file(
                "rbph-theme.json",
                json!({
                    "type":"rbph-theme", "apiVersion":1,
                    "package":{"name":"features","version":"1.0.0"},
                    "features":{
                        "locale":{"locales":{
                            "en":{"type":"inline","messages":{"theme":{"example":"Example"}}},
                            "ja":{"type":"module","source":"features/locales/ja.js"},
                            "zh-CN":{"type":"json","source":"features/locales/zh-CN.json"}
                        }},
                        "icons":{"collections":[
                            {"prefix":"example","icons":{"marker":{"body":"<path/>"}}}
                        ]},
                        "ui":{"icons":{"judge.milestone":"example:marker"}}
                    }
                }),
            ),
            file(
                "features/locales/zh-CN.json",
                json!({"judge":{"milestone":"路标"}}),
            ),
            AssetUploadFile {
                relative_path: "features/locales/ja.js".to_string(),
                bytes: b"export default {};".to_vec(),
                mime_type: "text/javascript; charset=utf-8".to_string(),
            },
        ];
        assert!(validate_theme_files(&files).is_some());
    }

    #[test]
    fn validates_external_icon_and_ui_files() {
        let file = |relative_path: &str, value: serde_json::Value| AssetUploadFile {
            relative_path: relative_path.to_string(),
            bytes: serde_json::to_vec(&value).unwrap(),
            mime_type: "application/json".to_string(),
        };
        let files = vec![
            file(
                "rbph-theme.json",
                json!({
                    "type":"rbph-theme", "apiVersion":1,
                    "package":{"name":"features","version":"1.0.0"},
                    "features":{
                        "icons":{"collections":[
                            {"prefix":"inline","icons":{"marker":{"body":"<path/>"}}},
                            "features/icons/collection-1.json"
                        ]},
                        "ui":{"source":"features/ui.json"}
                    }
                }),
            ),
            file(
                "features/icons/collection-1.json",
                json!({"prefix":"external","icons":{"marker":{"body":"<path/>"}}}),
            ),
            file(
                "features/ui.json",
                json!({"icons":{"judge.milestone":"external:marker"}}),
            ),
        ];
        assert!(validate_theme_files(&files).is_some());
    }

    #[test]
    fn rejects_invalid_inline_features() {
        let manifest = |features| {
            serde_json::from_value::<ThemeManifest>(json!({
                "type":"rbph-theme", "apiVersion":1,
                "package":{"name":"features","version":"1.0.0"},
                "features":features
            }))
            .unwrap()
        };
        assert!(
            package_paths(&manifest(json!({
                "icons":{"collections":[{"prefix":"example","icons":{"marker":{}}}]}
            })))
            .is_none()
        );
        assert!(
            package_paths(&manifest(json!({
                "ui":{"icons":{}}
            })))
            .is_none()
        );
        assert!(
            package_paths(&manifest(json!({
                "ui":{"source":"features/ui.json","icons":{"judge.milestone":"example:marker"}}
            })))
            .is_none()
        );
    }
}

use crate::{
    AppState,
    config::{StorageBackendConfig, UploadConfig},
    db,
    error::RbError,
    extractor::auth::AuthUser,
    module::storage::{
        AssetUploadFile, StoredAssetFile, StoredAssetGroup, build_public_path,
        sanitize_relative_path,
    },
};
use actix_web::{HttpResponse, Result, web};
use base64::{Engine, engine::general_purpose::STANDARD};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_repr::{Deserialize_repr, Serialize_repr};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

const PART_BYTES: u64 = 8 * 1024 * 1024;

#[repr(i16)]
#[derive(Clone, Copy, Deserialize_repr, IntoPrimitive, PartialEq, Serialize_repr)]
enum UploadPurpose {
    Asset = 0,
    Theme = 1,
}

#[repr(i16)]
#[derive(
    Clone, Copy, Deserialize_repr, IntoPrimitive, PartialEq, Serialize_repr, TryFromPrimitive,
)]
pub(super) enum UploadMode {
    File = 0,
    Group = 1,
}

#[repr(i16)]
#[derive(Clone, Copy, IntoPrimitive, Serialize_repr, TryFromPrimitive)]
enum UploadState {
    Uploading = 0,
    Confirming = 1,
    Failed = 2,
    Complete = 3,
    Cancelled = 4,
    Cleaned = 5,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct FileSpec {
    relative_path: String,
    size: u64,
    mime_type: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct CreateRequest {
    request_id: String,
    purpose: UploadPurpose,
    mode: UploadMode,
    game_id: i32,
    puzzle_id: Option<i32>,
    round_id: Option<i32>,
    backend: String,
    original_name: String,
    source_sha256: String,
    source_size: u64,
    files: Vec<FileSpec>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ExpectedPart {
    md5: String,
    size: u64,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct FileState {
    upload_id: Option<String>,
    #[serde(default)]
    parts: BTreeMap<u32, ExpectedPart>,
    sha256: Option<String>,
    #[serde(default)]
    completing: bool,
    #[serde(default)]
    complete: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Document {
    request: CreateRequest,
    object_key: String,
    files: Vec<FileState>,
}

struct Upload {
    id: String,
    state: UploadState,
    document: Document,
    result: Option<Value>,
    error: Option<String>,
    expires: i64,
    conn: sqlx::pool::PoolConnection<sqlx::Postgres>,
}

fn invalid(message: &str) -> actix_web::Error {
    RbError::bad_req(super::asset::AssetAdminResult::Invalid.into())
        .msg(message)
        .into()
}

fn internal(e: impl std::fmt::Debug) -> actix_web::Error {
    RbError::internal(e).into()
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn backend_limits(app: &AppState, backend: &str) -> Option<UploadConfig> {
    match app.settings.storage.backends.get(backend)? {
        StorageBackendConfig::Cos { upload, .. } => Some(upload.clone()),
        _ => None,
    }
}

fn validate_manifest(r: &CreateRequest, l: &UploadConfig) -> Result<()> {
    if (r.mode == UploadMode::File && (r.files.len() != 1 || r.source_size > l.max_file_bytes))
        || (r.purpose == UploadPurpose::Theme && r.mode != UploadMode::Group)
        || r.request_id.len() > 64
        || uuid::Uuid::parse_str(&r.request_id).is_err()
        || !is_sha256(&r.source_sha256)
        || r.original_name.trim().is_empty()
        || r.original_name.chars().count() > 255
        || r.files.is_empty()
        || r.files.len() > l.max_files
        || r.source_size > l.max_group_bytes
        || (r.puzzle_id.is_some() && r.round_id.is_some())
        || (r.purpose == UploadPurpose::Theme && (r.puzzle_id.is_some() || r.round_id.is_some()))
    {
        return Err(invalid("invalid upload manifest"));
    }

    let mut paths = HashSet::new();
    let mut size = 0u64;

    for f in &r.files {
        if !valid_file_spec(f, l, &mut paths) {
            return Err(invalid("invalid or duplicate file path, size or MIME type"));
        }

        size = size
            .checked_add(f.size)
            .ok_or_else(|| invalid("upload too large"))?;
        if size > l.max_group_bytes {
            return Err(invalid("upload exceeds group limit"));
        }
    }

    for path in &paths {
        for (index, _) in path.match_indices('/') {
            if paths.contains(&path[..index]) {
                return Err(invalid("file and directory paths conflict"));
            }
        }
    }

    Ok(())
}

fn valid_file_spec<'a>(
    f: &'a FileSpec,
    limits: &UploadConfig,
    paths: &mut HashSet<&'a str>,
) -> bool {
    !f.relative_path.is_empty()
        && f.relative_path.chars().count() <= 1024
        && sanitize_relative_path(&f.relative_path) == f.relative_path
        && !f
            .relative_path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !f.relative_path.contains('\\')
        && f.size <= limits.max_file_bytes
        && f.mime_type.len() <= 120
        && reqwest::header::HeaderValue::from_str(&f.mime_type).is_ok()
        && f.mime_type.contains('/')
        && paths.insert(f.relative_path.as_str())
}

async fn validate_scope(app: &AppState, r: &CreateRequest) -> Result<()> {
    if !db::game::exists(&app.db, r.game_id, crate::model::user::RbUserRole::Admin).await? {
        return Err(RbError::not_found().into());
    }
    if let Some(id) = r.puzzle_id
        && db::puzzle::get_puzzle_game(&app.db, id).await? != Some(r.game_id)
    {
        return Err(invalid("puzzle does not belong to game"));
    }
    if let Some(id) = r.round_id
        && db::round::get_round_game(&app.db, id).await? != Some(r.game_id)
    {
        return Err(invalid("round does not belong to game"));
    }

    Ok(())
}

impl Upload {
    async fn lock(pool: &crate::DbPool, id: &str, owner: Option<i32>) -> Result<Self> {
        let mut conn = pool.acquire().await.map_err(internal)?;

        // Closing on cancellation/error guarantees advisory locks cannot leak into the pool.
        conn.close_on_drop();

        let acquired: bool = sqlx::query_scalar!(
            "SELECT pg_try_advisory_lock(hashtextextended($1, 78341)) AS \"acquired!\"",
            id
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(internal)?;
        if !acquired {
            return Err(
                RbError::conflict(super::asset::AssetAdminResult::Invalid.into())
                    .msg("upload busy; retry shortly")
                    .into(),
            );
        }

        let row = sqlx::query!(
            r#"
                SELECT *, EXTRACT(EPOCH FROM expires_at)::bigint AS "expires!"
                FROM rb_asset_upload
                WHERE id = $1
                  AND ($2::int IS NULL OR owner_id = $2)
            "#,
            id,
            owner
        )
        .fetch_optional(&mut *conn)
        .await
        .map_err(internal)?
        .ok_or_else(RbError::not_found)?;

        Ok(Self {
            id: id.into(),
            state: UploadState::try_from(row.state).map_err(internal)?,
            document: serde_json::from_value(row.document).map_err(internal)?,
            result: row.result,
            error: row.error,
            expires: row.expires,
            conn,
        })
    }

    fn writable(&self) -> Result<()> {
        if !matches!(self.state, UploadState::Uploading)
            || self.expires <= time::OffsetDateTime::now_utc().unix_timestamp()
        {
            return Err(
                RbError::conflict(super::asset::AssetAdminResult::Invalid.into())
                    .msg("upload is no longer writable")
                    .into(),
            );
        }

        Ok(())
    }

    async fn save(&mut self) -> Result<()> {
        sqlx::query!(
            r#"
                UPDATE rb_asset_upload
                SET state = $2,
                    document = $3,
                    result = $4,
                    error = $5
                WHERE id = $1
            "#,
            &self.id,
            i16::from(self.state),
            json!(self.document),
            self.result.as_ref(),
            self.error.as_deref()
        )
        .execute(&mut *self.conn)
        .await
        .map_err(internal)?;

        Ok(())
    }

    async fn touch(&mut self) -> Result<()> {
        self.expires = sqlx::query_scalar!(
            r#"
                UPDATE rb_asset_upload
                SET touched_at = now(),
                    expires_at = LEAST(
                        created_at + INTERVAL '7 days',
                        now() + INTERVAL '24 hours'
                    )
                WHERE id = $1
                RETURNING EXTRACT(EPOCH FROM expires_at)::bigint AS "expires!"
            "#,
            &self.id
        )
        .fetch_one(&mut *self.conn)
        .await
        .map_err(internal)?;

        Ok(())
    }

    fn response(&self) -> Value {
        json!({
            "code": super::asset::AssetAdminResult::Ok,
            "id": self.id,
            "state": self.state,
            "request": self.document.request,
            "files": self.document.files,
            "result": self.result,
            "error": self.error,
            "expires_at": self.expires,
            "part_bytes": PART_BYTES,
        })
    }
}

async fn create(
    body: web::Json<CreateRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let r = body.into_inner();
    let old: Option<String> = sqlx::query_scalar!(
        "SELECT id FROM rb_asset_upload WHERE owner_id = $1 AND request_id = $2",
        user.uid,
        &r.request_id
    )
    .fetch_optional(&app.db)
    .await
    .map_err(internal)?;
    if let Some(id) = old {
        let u = Upload::lock(&app.db, &id, Some(user.uid)).await?;
        if u.document.request != r {
            return Err(invalid("request id reused with different manifest"));
        }
        return Ok(HttpResponse::Ok().json(u.response()));
    }

    let l = backend_limits(&app, &r.backend)
        .filter(|l| l.direct)
        .ok_or_else(|| invalid("direct upload is disabled for this backend"))?;
    validate_manifest(&r, &l)?;
    validate_scope(&app, &r).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let doc = Document {
        object_key: format!("group-{id}"),
        files: vec![FileState::default(); r.files.len()],
        request: r.clone(),
    };

    let actual: String = sqlx::query_scalar!(
        r#"
            INSERT INTO rb_asset_upload (id, owner_id, request_id, game_id, document)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (owner_id, request_id) DO UPDATE
            SET request_id = EXCLUDED.request_id
            RETURNING id
        "#,
        &id,
        user.uid,
        &r.request_id,
        r.game_id,
        json!(doc)
    )
    .fetch_one(&app.db)
    .await
    .map_err(internal)?;

    let u = Upload::lock(&app.db, &actual, Some(user.uid)).await?;
    if u.document.request != r {
        return Err(invalid("request id reused with different manifest"));
    }

    Ok(HttpResponse::Ok().json(u.response()))
}

#[derive(Deserialize)]
struct ListQuery {
    game_id: i32,
}

async fn list(
    q: web::Query<ListQuery>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let rows = sqlx::query!(
        r#"
            SELECT id,
                   state,
                   document->'request' AS "request!",
                   (
                       SELECT count(*)
                       FROM jsonb_array_elements(document->'files') AS file
                       WHERE file->>'complete' = 'true'
                   ) AS "completed_files!",
                   error
            FROM rb_asset_upload
            WHERE owner_id = $1
              AND game_id = $2
              AND state IN ($3, $4, $5)
              AND expires_at > now()
            ORDER BY created_at DESC
            LIMIT 100
        "#,
        user.uid,
        q.game_id,
        i16::from(UploadState::Uploading),
        i16::from(UploadState::Confirming),
        i16::from(UploadState::Failed)
    )
    .fetch_all(&app.db)
    .await
    .map_err(internal)?;

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
        "uploads": rows.iter().map(|r| json!({
            "id": r.id,
            "state": r.state,
            "request": r.request,
            "completed_files": r.completed_files,
            "error": r.error,
        })).collect::<Vec<_>>(),
    })))
}

async fn get(
    id: web::Path<String>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let row = sqlx::query!(
        r#"
            SELECT *,
                   EXTRACT(EPOCH FROM expires_at)::bigint AS "expires!"
            FROM rb_asset_upload
            WHERE id = $1
              AND owner_id = $2
        "#,
        id.as_str(),
        user.uid
    )
    .fetch_optional(&app.db)
    .await
    .map_err(internal)?
    .ok_or_else(RbError::not_found)?;

    let doc: Document = serde_json::from_value(row.document).map_err(internal)?;

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
        "id": id.as_str(),
        "state": row.state,
        "request": doc.request,
        "files": doc.files,
        "result": row.result,
        "error": row.error,
        "expires_at": row.expires,
        "part_bytes": PART_BYTES,
    })))
}

#[derive(Deserialize)]
struct FilePath {
    id: String,
    file_id: usize,
}

#[derive(Deserialize)]
struct PartRequest {
    part_number: u32,
    content_md5: String,
}

async fn parts(
    path: web::Path<FilePath>,
    body: web::Json<PartRequest>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut u = Upload::lock(&app.db, &path.id, Some(user.uid)).await?;
    u.writable()?;

    let spec = u
        .document
        .request
        .files
        .get(path.file_id)
        .cloned()
        .ok_or_else(|| invalid("unknown file"))?;
    if STANDARD
        .decode(&body.content_md5)
        .ok()
        .is_none_or(|v| v.len() != 16)
        || body.part_number == 0
        || u64::from(body.part_number) > spec.size.div_ceil(PART_BYTES)
    {
        return Err(invalid("invalid part number or MD5"));
    }

    let state = &u.document.files[path.file_id];
    if state.complete || state.completing {
        return Err(invalid("file is already completing"));
    }
    if state
        .parts
        .get(&body.part_number)
        .is_some_and(|p| p.md5 != body.content_md5)
    {
        return Err(invalid("part content changed; start a new upload"));
    }

    let cos = app.storage.cos(&u.document.request.backend)?;
    if state.upload_id.is_none() {
        let pending = cos
            .pending(&u.document.object_key, &spec.relative_path)
            .await?;

        let upload = if let Some(id) = pending.first() {
            id.clone()
        } else {
            cos.initiate(
                &u.document.object_key,
                &spec.relative_path,
                &spec.mime_type,
                &u.id,
            )
            .await?
        };

        for extra in pending.iter().skip(1) {
            cos.abort(&u.document.object_key, &spec.relative_path, extra)
                .await?;
        }
        u.document.files[path.file_id].upload_id = Some(upload);
        u.save().await?;
    }

    let offset = (u64::from(body.part_number) - 1) * PART_BYTES;
    let size = (spec.size - offset).min(PART_BYTES);

    u.document.files[path.file_id].parts.insert(
        body.part_number,
        ExpectedPart {
            md5: body.content_md5.clone(),
            size,
        },
    );
    u.touch().await?;
    u.save().await?;

    let expires = (time::OffsetDateTime::now_utc().unix_timestamp() + 900).min(u.expires) as u64;
    let upload_id = u.document.files[path.file_id]
        .upload_id
        .as_deref()
        .expect("upload ID is initialized above");

    let auth = cos.authorize_part(
        &u.document.object_key,
        &spec.relative_path,
        upload_id,
        body.part_number,
        (&body.content_md5, size),
        expires,
    );

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
        "part": auth,
    })))
}

async fn list_parts(
    path: web::Path<FilePath>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut u = Upload::lock(&app.db, &path.id, Some(user.uid)).await?;
    let spec = u
        .document
        .request
        .files
        .get(path.file_id)
        .ok_or_else(|| invalid("unknown file"))?;

    let state = &u.document.files[path.file_id];
    let parts = if let Some(upload) = &state.upload_id {
        if !state.complete && !state.completing {
            app.storage
                .cos(&u.document.request.backend)?
                .parts(&u.document.object_key, &spec.relative_path, upload)
                .await?
        } else {
            vec![]
        }
    } else {
        vec![]
    };
    let writable = matches!(u.state, UploadState::Uploading)
        && u.expires > time::OffsetDateTime::now_utc().unix_timestamp();
    if writable {
        u.touch().await?;
    }

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
        "parts": parts,
        "file": u.document.files[path.file_id],
    })))
}

#[derive(Deserialize)]
struct FileComplete {
    sha256: String,
}

fn verify_parts(
    size: u64,
    expected: &BTreeMap<u32, ExpectedPart>,
    actual: &[crate::module::storage::direct::Part],
) -> Result<()> {
    if actual.len() as u64 != size.div_ceil(PART_BYTES) {
        return Err(invalid("file is missing parts"));
    }

    for (index, part) in actual.iter().enumerate() {
        let wanted = expected
            .get(&part.part_number)
            .ok_or_else(|| invalid("unexpected COS part"))?;

        let md5 = STANDARD
            .decode(&wanted.md5)
            .map_err(|_| invalid("invalid part digest"))?
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();

        let required_size = (size - index as u64 * PART_BYTES).min(PART_BYTES);
        if part.part_number != index as u32 + 1
            || part.size != wanted.size
            || part.size != required_size
            || part.etag.trim_matches('"').to_ascii_lowercase() != md5
        {
            return Err(invalid("COS part verification failed"));
        }
    }

    Ok(())
}

async fn complete_file(
    path: web::Path<FilePath>,
    body: web::Json<FileComplete>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut u = Upload::lock(&app.db, &path.id, Some(user.uid)).await?;
    let spec = u
        .document
        .request
        .files
        .get(path.file_id)
        .cloned()
        .ok_or_else(|| invalid("unknown file"))?;
    if !is_sha256(&body.sha256) {
        return Err(invalid("invalid SHA-256"));
    }
    if u.document.files[path.file_id]
        .sha256
        .as_ref()
        .is_some_and(|s| s != &body.sha256)
    {
        return Err(invalid("file digest changed"));
    }
    if u.document.files[path.file_id].complete {
        return Ok(HttpResponse::Ok().json(json!({
            "code": super::asset::AssetAdminResult::Ok,
        })));
    }

    u.writable()?;

    let cos = app.storage.cos(&u.document.request.backend)?;
    let state = u.document.files[path.file_id].clone();

    let recovered = if state.completing {
        cos.head(&u.document.object_key, &spec.relative_path)
            .await?
            .is_some_and(|h| h.size == spec.size && h.session.as_deref() == Some(u.id.as_str()))
    } else {
        false
    };
    if !recovered {
        if spec.size == 0 {
            if body.sha256 != format!("{:x}", Sha256::digest([])) {
                return Err(invalid("invalid empty file digest"));
            }
            u.document.files[path.file_id].sha256 = Some(body.sha256.clone());
            u.document.files[path.file_id].completing = true;
            u.save().await?;
            cos.empty(
                &u.document.object_key,
                &spec.relative_path,
                &spec.mime_type,
                &u.id,
            )
            .await?;
        } else {
            let upload = state
                .upload_id
                .ok_or_else(|| invalid("file has no uploaded parts"))?;

            let mut actual = cos
                .parts(&u.document.object_key, &spec.relative_path, &upload)
                .await?;
            actual.sort_by_key(|p| p.part_number);
            verify_parts(spec.size, &state.parts, &actual)?;
            u.document.files[path.file_id].sha256 = Some(body.sha256.clone());
            u.document.files[path.file_id].completing = true;
            u.save().await?;
            cos.finish(
                &u.document.object_key,
                &spec.relative_path,
                &upload,
                &actual,
            )
            .await?;
        }
    }

    let head = cos
        .head(&u.document.object_key, &spec.relative_path)
        .await?
        .ok_or_else(|| invalid("completed object missing"))?;
    if head.size != spec.size || head.session.as_deref() != Some(u.id.as_str()) {
        return Err(invalid("completed object does not match upload"));
    }
    u.document.files[path.file_id].complete = true;
    u.touch().await?;
    u.save().await?;

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
    })))
}

async fn complete(
    id: web::Path<String>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut u = Upload::lock(&app.db, &id, Some(user.uid)).await?;
    if matches!(u.state, UploadState::Complete | UploadState::Confirming) {
        return Ok(HttpResponse::Ok().json(u.response()));
    }
    if matches!(u.state, UploadState::Failed)
        && u.expires > time::OffsetDateTime::now_utc().unix_timestamp()
    {
        u.state = UploadState::Uploading;
    }
    u.writable()?;
    if u.document.files.iter().any(|f| !f.complete) {
        return Err(invalid("upload contains incomplete files"));
    }
    validate_scope(&app, &u.document.request).await?;
    u.state = UploadState::Confirming;
    u.error = None;
    u.touch().await?;
    u.save().await?;

    Ok(HttpResponse::Accepted().json(u.response()))
}

async fn cancel(
    id: web::Path<String>,
    user: AuthUser,
    app: web::Data<AppState>,
) -> Result<HttpResponse> {
    let mut u = Upload::lock(&app.db, &id, Some(user.uid)).await?;
    if matches!(u.state, UploadState::Complete) {
        return Err(invalid(
            "registered assets must be deleted through asset management",
        ));
    }
    if !matches!(u.state, UploadState::Cleaned) {
        u.state = UploadState::Cancelled;
        u.save().await?;
    }

    Ok(HttpResponse::Ok().json(json!({
        "code": super::asset::AssetAdminResult::Ok,
    })))
}

pub(super) fn manifest_digest(files: &[StoredAssetFile]) -> String {
    let mut sorted = files.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.relative_path.as_bytes().cmp(b.relative_path.as_bytes()));

    let mut h = Sha256::new();
    h.update(b"rbph:manifest-v1\0");

    for f in sorted {
        h.update((f.relative_path.len() as u64).to_le_bytes());
        h.update(f.relative_path.as_bytes());
        h.update(f.size.to_le_bytes());

        for pair in f.sha256.as_bytes().chunks_exact(2) {
            let digit = |v: u8| if v <= b'9' { v - b'0' } else { v - b'a' + 10 };
            h.update([digit(pair[0]) * 16 + digit(pair[1])]);
        }
    }
    format!("{:x}", h.finalize())
}

fn verify_metadata_bytes(files: &[StoredAssetFile], path: &str, bytes: &[u8]) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !files.iter().any(|file| {
        file.relative_path == path && file.size == bytes.len() as u64 && file.sha256 == actual
    }) {
        return Err(invalid("theme metadata changed after content verification"));
    }

    Ok(())
}

async fn register(app: &AppState, u: &mut Upload) -> Result<()> {
    let r = &u.document.request;
    validate_scope(app, r).await?;

    let cos = app.storage.cos(&r.backend)?;
    let mut files = Vec::with_capacity(r.files.len());

    for (spec, state) in r.files.iter().zip(&u.document.files) {
        if !state.complete {
            return Err(invalid("file is not complete"));
        }

        let claimed = state
            .sha256
            .as_deref()
            .filter(|s| is_sha256(s))
            .ok_or_else(|| invalid("file digest missing"))?;

        let actual = cos
            .verified_sha256(
                &u.document.object_key,
                &spec.relative_path,
                spec.size,
                &u.id,
                claimed,
            )
            .await?;

        files.push(StoredAssetFile {
            relative_path: spec.relative_path.clone(),
            size: spec.size,
            sha256: actual,
            mime_type: spec.mime_type.clone(),
            path: build_public_path(&u.document.object_key, &spec.relative_path),
        });
    }

    let stored = StoredAssetGroup {
        size: files.iter().map(|f| f.size).sum(),
        sha256: manifest_digest(&files),
        files,
    };

    let manifest = if r.purpose == UploadPurpose::Theme {
        let cos = app.storage.cos(&r.backend)?;
        let bytes = cos
            .metadata(&u.document.object_key, "rbph-theme.json")
            .await?;
        verify_metadata_bytes(&stored.files, "rbph-theme.json", &bytes)?;

        let m: db::frontend::ThemeManifest =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid theme manifest"))?;

        let paths =
            super::frontend::package_paths(&m).ok_or_else(|| invalid("invalid theme paths"))?;

        let known = r
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<HashSet<_>>();
        if paths.iter().any(|p| !known.contains(p.as_str())) {
            return Err(invalid("theme references missing files"));
        }

        let mut metadata = BTreeMap::from([("rbph-theme.json".to_string(), bytes)]);
        let mut wanted = HashSet::new();
        if let Some(locale) = &m.features.locale {
            for entry in locale.locales.values() {
                if let db::frontend::ThemeLocaleEntry::Json { source } = entry {
                    wanted.insert(source.clone());
                }
            }
        }
        if let Some(icons) = &m.features.icons {
            for item in &icons.collections {
                if let Some(path) = item.as_str() {
                    wanted.insert(path.to_string());
                }
            }
        }
        if let Some(ui) = &m.features.ui
            && let Some(source) = &ui.source
        {
            wanted.insert(source.clone());
        }

        for path in wanted {
            let path = super::frontend::normalize_relative_path(&path)
                .ok_or_else(|| invalid("invalid theme metadata path"))?
                .to_string();

            let bytes = cos.metadata(&u.document.object_key, &path).await?;
            verify_metadata_bytes(&stored.files, &path, &bytes)?;
            metadata.insert(path, bytes);
        }

        // Existing validator checks metadata content and uses the complete path inventory.
        let input = r
            .files
            .iter()
            .map(|f| AssetUploadFile {
                relative_path: f.relative_path.clone(),
                mime_type: f.mime_type.clone(),
                bytes: metadata.remove(&f.relative_path).unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        Some(
            super::frontend::validate_theme_files(&input)
                .ok_or_else(|| invalid("invalid theme metadata"))?,
        )
    } else {
        None
    };

    let mut tx = app.db.begin().await.map_err(internal)?;
    let (group_id, result) = if let Some(manifest) = manifest {
        let (package, replaced) = super::frontend::register_uploaded_theme(
            &mut tx,
            r.game_id,
            &r.backend,
            &u.document.object_key,
            &r.original_name,
            &stored,
            &manifest,
        )
        .await?;
        if let Some((group, files)) = replaced {
            sqlx::query!(
                "INSERT INTO rb_asset_upload_garbage (backend, object_key, paths)
                 VALUES ($1, $2, $3)",
                group.backend,
                group.object_key,
                json!(files.iter().map(|f| &f.relative_path).collect::<Vec<_>>())
            )
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        }

        let group_id = package.asset_group_id;
        let mut package = json!(package);
        package["digest_version"] = json!("manifest-v1");
        package["sha256_source"] = json!("server");
        (group_id, json!({"package": package}))
    } else {
        let group = db::asset::create_group_conn(
            &mut tx,
            db::asset::CreateAssetGroupData {
                game_id: r.game_id,
                puzzle_id: r.puzzle_id,
                round_id: r.round_id,
                backend: &r.backend,
                object_key: &u.document.object_key,
                original_name: &r.original_name,
                mime_type: if r.mode == UploadMode::File {
                    &r.files[0].mime_type
                } else {
                    "application/zip"
                },
                size: stored.size as i64,
                sha256: &stored.sha256,
            },
        )
        .await?;

        let mut files = Vec::new();

        for f in &stored.files {
            files.push(
                db::asset::create_file_conn(
                    &mut tx,
                    group.id,
                    &f.relative_path,
                    &f.mime_type,
                    f.size as i64,
                    &f.sha256,
                )
                .await?,
            );
        }

        let group_id = group.id;
        let public_url = app
            .storage
            .asset_group_public_url(&group.backend, &group.object_key);

        let files = files
            .into_iter()
            .map(|file| {
                let public_url = app.storage.asset_public_url(
                    &group.backend,
                    &group.object_key,
                    &file.relative_path,
                );

                let mut file = json!(file);
                file["public_url"] = json!(public_url);
                file["sha256_source"] = json!("server");
                file
            })
            .collect::<Vec<_>>();

        let mut group = json!(group);
        group["public_url"] = json!(public_url);
        group["digest_version"] = json!("manifest-v1");
        group["sha256_source"] = json!("server");
        (group_id, json!({"group": group, "files": files}))
    };
    sqlx::query!(
        "INSERT INTO rb_asset_digest (group_id, digest_version, sha256_source)
         VALUES ($1, 'manifest-v1', 'server')",
        group_id
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    sqlx::query!(
        "UPDATE rb_asset_upload
         SET state = $3,
             result = $2,
             error = NULL
         WHERE id = $1",
        &u.id,
        &result,
        i16::from(UploadState::Complete)
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    u.state = UploadState::Complete;
    u.result = Some(result);

    Ok(())
}

async fn cleanup(app: &AppState, u: &mut Upload) -> Result<()> {
    let cos = app.storage.cos(&u.document.request.backend)?;

    for spec in &u.document.request.files {
        for upload_id in cos
            .pending(&u.document.object_key, &spec.relative_path)
            .await?
        {
            cos.abort(&u.document.object_key, &spec.relative_path, &upload_id)
                .await?;
        }
    }

    let paths = u
        .document
        .request
        .files
        .iter()
        .map(|f| f.relative_path.clone())
        .collect::<Vec<_>>();
    app.storage
        .delete_files(&u.document.request.backend, &u.document.object_key, &paths)
        .await?;
    u.state = UploadState::Cleaned;
    u.save().await
}

async fn tick(app: &AppState, clean: bool) -> Result<()> {
    let rows = sqlx::query!(
        "SELECT id, state
         FROM rb_asset_upload
         WHERE state = $2
           OR (
               $1
               AND (
                    state = $3
                    OR (state IN ($4, $5) AND expires_at <= now())
               )
           )
         ORDER BY created_at
         LIMIT 50",
        clean,
        i16::from(UploadState::Confirming),
        i16::from(UploadState::Cancelled),
        i16::from(UploadState::Uploading),
        i16::from(UploadState::Failed)
    )
    .fetch_all(&app.db)
    .await
    .map_err(internal)?;

    for row in rows {
        let id: String = row.id;
        let Ok(mut u) = Upload::lock(&app.db, &id, None).await else {
            continue;
        };
        if matches!(u.state, UploadState::Confirming) {
            let started = std::time::Instant::now();
            let registration = register(app, &mut u).await.map_err(|e| e.to_string());
            if let Err(error) = registration {
                // Re-read after an ambiguous commit before changing state.
                let state: i16 =
                    sqlx::query_scalar!("SELECT state FROM rb_asset_upload WHERE id = $1", &id)
                        .fetch_one(&mut *u.conn)
                        .await
                        .map_err(internal)?;
                if state != i16::from(UploadState::Complete) {
                    u.state = UploadState::Failed;
                    u.error = Some("Upload confirmation failed; retry or cancel the task".into());
                    u.save().await?;
                }
                log::warn!("COS upload {id} confirmation failed: {error}");
            } else {
                log::info!("COS upload {id} confirmed in {:?}", started.elapsed());
            }

            // Invalidation is repeated for complete theme sessions by the durable worker below.
        } else if (matches!(u.state, UploadState::Cancelled)
            || (matches!(u.state, UploadState::Uploading | UploadState::Failed)
                && u.expires <= time::OffsetDateTime::now_utc().unix_timestamp()))
            && let Err(error) = cleanup(app, &mut u).await
        {
            log::warn!("COS upload {id} cleanup failed: {error}");
        }
    }

    // Cache invalidation is journaled independently of registration/HTTP responses.
    let themes = sqlx::query!(
        "SELECT id, game_id AS \"game_id!\"
         FROM rb_asset_upload
         WHERE state = $1
           AND game_id IS NOT NULL
           AND document->'request'->'purpose' = to_jsonb($2::smallint)
           AND NOT (document ? 'cache_done')
         LIMIT 50",
        i16::from(UploadState::Complete),
        i16::from(UploadPurpose::Theme)
    )
    .fetch_all(&app.db)
    .await
    .map_err(internal)?;

    for row in themes {
        let id: String = row.id;
        let game: i32 = row.game_id;
        if db::frontend::invalidate_renderer_cache(&app.kv, game, None)
            .await
            .is_ok()
        {
            sqlx::query!(
                "UPDATE rb_asset_upload
                 SET document = jsonb_set(document, '{cache_done}', 'true')
                 WHERE id = $1",
                id
            )
            .execute(&app.db)
            .await
            .map_err(internal)?;
        }
    }
    if clean {
        let rows = sqlx::query!("SELECT * FROM rb_asset_upload_garbage LIMIT 100")
            .fetch_all(&app.db)
            .await
            .map_err(internal)?;

        for row in rows {
            let paths: Vec<String> = serde_json::from_value(row.paths).map_err(internal)?;
            if app
                .storage
                .delete_files(&row.backend, &row.object_key, &paths)
                .await
                .is_ok()
            {
                sqlx::query!(
                    "DELETE FROM rb_asset_upload_garbage
                     WHERE id = $1",
                    row.id
                )
                .execute(&app.db)
                .await
                .map_err(internal)?;
            }
        }
    }

    Ok(())
}

pub async fn run(app: AppState) {
    let mut ticks = 0u64;

    loop {
        if let Err(e) = tick(&app, ticks.is_multiple_of(300)).await {
            log::warn!("COS upload worker failed: {e}");
        }
        ticks += 1;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

pub(super) fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/uploads")
            .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024))
            .route("", web::post().to(create))
            .route("", web::get().to(list))
            .route("/{id}", web::get().to(get))
            .route("/{id}", web::delete().to(cancel))
            .route("/{id}/complete", web::post().to(complete))
            .route("/{id}/files/{file_id}/parts", web::post().to(parts))
            .route("/{id}/files/{file_id}/parts", web::get().to(list_parts))
            .route(
                "/{id}/files/{file_id}/complete",
                web::post().to(complete_file),
            ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> CreateRequest {
        CreateRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            purpose: UploadPurpose::Asset,
            mode: UploadMode::Group,
            game_id: 1,
            puzzle_id: None,
            round_id: None,
            backend: "cos".into(),
            original_name: "x.zip".into(),
            source_sha256: "a".repeat(64),
            source_size: 1,
            files: vec![FileSpec {
                relative_path: "foo/a.txt".into(),
                size: 1,
                mime_type: "text/plain".into(),
            }],
        }
    }

    #[test]
    fn rejects_conflicting_manifest_paths_and_exceeded_limits() {
        let original = request();
        validate_manifest(&original, &UploadConfig::default()).unwrap();

        for path in ["../outside.txt", "foo/a.txt", "foo"] {
            let mut manifest = original.clone();
            let mut file = manifest.files[0].clone();
            file.relative_path = path.into();
            manifest.files.push(file);
            assert!(validate_manifest(&manifest, &UploadConfig::default()).is_err());
        }

        let limits = UploadConfig {
            max_file_bytes: 1,
            max_group_bytes: 2,
            max_files: 1,
            ..UploadConfig::default()
        };
        let mut manifest = original;
        manifest.files[0].size = 2;
        assert!(validate_manifest(&manifest, &limits).is_err());
    }

    #[test]
    fn manifest_digest_is_stable_across_file_order() {
        let f = |p: &str| StoredAssetFile {
            relative_path: p.into(),
            size: 1,
            sha256: "a".repeat(64),
            mime_type: "text/plain".into(),
            path: p.into(),
        };
        assert_eq!(
            manifest_digest(&[f("b"), f("a")]),
            manifest_digest(&[f("a"), f("b")])
        );
    }

    #[test]
    fn verifies_part_manifest() {
        use crate::module::storage::direct::Part;

        let expected = BTreeMap::from([(
            1,
            ExpectedPart {
                md5: STANDARD.encode([0u8; 16]),
                size: 1,
            },
        )]);

        let part = Part {
            part_number: 1,
            etag: format!("\"{}\"", "0".repeat(32)),
            size: 1,
        };

        verify_parts(1, &expected, std::slice::from_ref(&part)).unwrap();
        assert!(verify_parts(1, &expected, &[]).is_err());
        assert!(
            verify_parts(
                1,
                &expected,
                &[Part {
                    size: 2,
                    ..part.clone()
                }]
            )
            .is_err()
        );
        assert!(
            verify_parts(
                1,
                &expected,
                &[Part {
                    etag: "1".repeat(32),
                    ..part.clone()
                }]
            )
            .is_err()
        );
        assert!(
            verify_parts(
                1,
                &expected,
                &[Part {
                    part_number: 2,
                    ..part
                }]
            )
            .is_err()
        );
    }

    #[sqlx::test(migrations = false)]
    async fn durable_session_persists_upload_progress(pool: sqlx::PgPool) {
        sqlx::raw_sql(
            "CREATE TABLE rb_user(id int primary key); CREATE TABLE rb_game(id int primary key); \
            CREATE TABLE rb_asset_group(id int primary key);",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/20260910000000_cos_direct_upload.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO rb_user VALUES(1); INSERT INTO rb_game VALUES(1);")
            .execute(&pool)
            .await
            .unwrap();

        let r = request();
        let doc = Document {
            object_key: "group-test".into(),
            files: vec![FileState::default()],
            request: r.clone(),
        };
        sqlx::query!(
            "INSERT INTO rb_asset_upload(id,owner_id,request_id,game_id,document) VALUES('test',1,$1,1,$2)",
            &r.request_id,
            json!(doc)
        )
        .execute(&pool)
        .await
        .unwrap();
        let error = Upload::lock(&pool, "test", Some(2)).await.err().unwrap();
        assert_eq!(
            error.as_response_error().error_response().status(),
            actix_web::http::StatusCode::NOT_FOUND
        );

        // Wait for PostgreSQL to release the advisory lock on the closed connection.
        let mut upload = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(upload) = Upload::lock(&pool, "test", Some(1)).await {
                    break upload;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let error = Upload::lock(&pool, "test", Some(1)).await.err().unwrap();
        assert_eq!(
            error.as_response_error().error_response().status(),
            actix_web::http::StatusCode::CONFLICT
        );

        upload.document.files[0].parts.insert(
            1,
            ExpectedPart {
                md5: STANDARD.encode([0; 16]),
                size: 1,
            },
        );
        upload.save().await.unwrap();

        let saved: Value =
            sqlx::query_scalar!("SELECT document FROM rb_asset_upload WHERE id='test'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(saved["files"][0]["parts"]["1"]["size"], 1);

        upload.writable().unwrap();
        upload.expires = 0;
        assert!(upload.writable().is_err());
        upload.expires = i64::MAX;
        upload.state = UploadState::Confirming;
        assert!(upload.writable().is_err());
    }

    #[test]
    fn verifies_theme_metadata_content() {
        let bytes = b"{}";
        let files = vec![StoredAssetFile {
            relative_path: "meta.json".into(),
            size: 2,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            mime_type: "application/json".into(),
            path: String::new(),
        }];
        verify_metadata_bytes(&files, "meta.json", bytes).unwrap();
        assert!(verify_metadata_bytes(&files, "meta.json", b"[]").is_err());
        assert!(verify_metadata_bytes(&files, "missing.json", bytes).is_err());
    }
}

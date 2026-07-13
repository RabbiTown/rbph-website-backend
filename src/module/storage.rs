use std::{
    collections::{HashMap, VecDeque},
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, Method, header};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use zip::read::ZipArchive;

use crate::error::RbInternalError;

use crate::config::{StorageBackendConfig, StorageConfig};

const URL_PATH_SEGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
const STORAGE_OBJECT_KEY_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_');
pub const DATABASE_MAX_FILE_BYTES: u64 = 1024 * 1024;
pub const DATABASE_MAX_GROUP_BYTES: u64 = 20 * 1024 * 1024;
pub const DATABASE_MAX_GROUP_FILES: usize = 1000;
const DATABASE_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const DATABASE_CACHE_IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct LocalStorage {
    root: Arc<PathBuf>,
}

#[derive(Clone)]
pub struct StorageManager {
    backends: Arc<HashMap<String, StorageInstance>>,
    default_backend: Arc<str>,
    database_cache: DatabaseAssetCache,
}

#[derive(Clone)]
struct StorageInstance {
    label: Arc<str>,
    backend: StorageBackend,
}

#[derive(Clone)]
enum StorageBackend {
    Local(LocalStorage),
    Cos(CosStorage),
    Database,
}

pub struct ConfiguredStorageBackend {
    pub id: String,
    pub kind: &'static str,
    pub label: String,
    pub recommended: bool,
    pub public_read: bool,
    pub backend_read: bool,
    pub allowed_scopes: &'static [&'static str],
    pub max_file_bytes: Option<u64>,
    pub max_group_bytes: Option<u64>,
}

#[derive(Clone)]
struct DatabaseAssetCache {
    inner: Arc<Mutex<DatabaseAssetCacheInner>>,
}

struct DatabaseAssetCacheInner {
    entries: HashMap<String, DatabaseAssetCacheEntry>,
    order: VecDeque<String>,
    bytes: u64,
}

struct DatabaseAssetCacheEntry {
    content: Arc<[u8]>,
    last_used: Instant,
}

#[derive(Clone)]
struct CosStorage {
    client: Client,
    host: Arc<str>,
    secret_id: Arc<str>,
    secret_key: Arc<str>,
    public_base_url: Arc<str>,
}

#[derive(Clone)]
pub struct StoredAsset {
    pub object_key: String,
    pub size: u64,
    pub sha256: String,
    pub mime_type: String,
    pub original_name: String,
    pub path: String,
}

#[derive(Clone)]
pub struct AssetUploadFile {
    pub relative_path: String,
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Clone)]
pub struct StoredAssetFile {
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
    pub mime_type: String,
    pub path: String,
}

#[derive(Clone)]
pub struct StoredPublicFile {
    pub size: u64,
    pub sha256: String,
}

pub struct StoredAssetGroup {
    pub size: u64,
    pub sha256: String,
    pub files: Vec<StoredAssetFile>,
}

pub struct StoredAssetGroupSummary {
    pub size: u64,
    pub sha256: String,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
        }
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub fn public_path(&self, object_key: &str, original_name: &str) -> String {
        build_public_path(object_key, original_name)
    }

    pub fn object_dir(&self, object_key: &str) -> PathBuf {
        self.root().join(sanitize_object_key(object_key))
    }

    pub fn object_path(&self, object_key: &str, relative_path: &str) -> PathBuf {
        self.object_dir(object_key)
            .join(sanitize_relative_path(relative_path))
    }

    pub fn temp_path(&self, object_key: &str) -> PathBuf {
        self.root()
            .join(format!("{}.tmp", sanitize_object_key(object_key)))
    }

    pub async fn store(
        &self,
        bytes: &[u8],
        original_name: &str,
        mime_type: &str,
    ) -> Result<StoredAsset, RbInternalError> {
        fs::create_dir_all(self.root()).await?;

        let object_key = format!("local-{}", uuid::Uuid::new_v4());
        let tmp_path = self.temp_path(&object_key);
        let final_path = self.object_path(&object_key, original_name);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut file = fs::File::create(&tmp_path).await?;
        file.write_all(bytes).await?;
        file.flush().await?;

        fs::rename(&tmp_path, &final_path).await?;

        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let sha256 = format!("{:x}", hasher.finalize());

        Ok(StoredAsset {
            object_key: object_key.clone(),
            size: bytes.len() as u64,
            sha256,
            mime_type: mime_type.to_string(),
            original_name: original_name.to_string(),
            path: self.public_path(&object_key, original_name),
        })
    }

    pub async fn store_group_files(
        &self,
        object_key: &str,
        files: &[AssetUploadFile],
    ) -> Result<StoredAssetGroup, RbInternalError> {
        fs::create_dir_all(self.root()).await?;

        let tmp_dir = self.temp_path(object_key);
        let final_dir = self.object_dir(object_key);
        if final_dir.exists() {
            fs::remove_dir_all(&final_dir).await.ok();
        }
        if tmp_dir.exists() {
            fs::remove_dir_all(&tmp_dir).await.ok();
        }
        fs::create_dir_all(&tmp_dir).await?;

        let mut stored_files = Vec::with_capacity(files.len());
        let mut used_paths: HashMap<String, usize> = HashMap::new();
        let mut group_hasher = Sha256::new();
        let mut group_size: u64 = 0;
        for file in files {
            let relative_path = uniquify_relative_path(
                sanitize_relative_path(&file.relative_path),
                &mut used_paths,
            );
            let file_path = tmp_dir.join(&relative_path);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).await?;
            }

            let mut handle = fs::File::create(&file_path).await?;
            handle.write_all(&file.bytes).await?;
            handle.flush().await?;

            let mut hasher = Sha256::new();
            hasher.update(&file.bytes);
            let file_size = file.bytes.len() as u64;
            group_hasher.update(relative_path.as_bytes());
            group_hasher.update([0]);
            group_hasher.update(file_size.to_le_bytes());
            group_hasher.update(&file.bytes);
            group_size += file_size;
            stored_files.push(StoredAssetFile {
                relative_path: relative_path.clone(),
                size: file_size,
                sha256: format!("{:x}", hasher.finalize()),
                mime_type: file.mime_type.clone(),
                path: self.public_path(object_key, &relative_path),
            });
        }

        fs::rename(&tmp_dir, &final_dir).await?;
        Ok(StoredAssetGroup {
            size: group_size,
            sha256: format!("{:x}", group_hasher.finalize()),
            files: stored_files,
        })
    }

    pub async fn store_public_file(
        &self,
        object_key: &str,
        relative_path: &str,
        bytes: &[u8],
        _mime_type: &str,
        _cache_control: Option<&str>,
    ) -> Result<StoredPublicFile, RbInternalError> {
        fs::create_dir_all(self.root()).await?;

        let final_path = self.object_path(object_key, relative_path);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let tmp_path = self.temp_path(object_key);
        let mut file = fs::File::create(&tmp_path).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        fs::rename(&tmp_path, &final_path).await?;

        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Ok(StoredPublicFile {
            size: bytes.len() as u64,
            sha256: format!("{:x}", hasher.finalize()),
        })
    }

    pub fn unpack_zip_files_limited(
        bytes: &[u8],
        max_file_bytes: u64,
        max_group_bytes: u64,
        max_files: usize,
    ) -> Result<Vec<AssetUploadFile>, RbInternalError> {
        let cursor = Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor)?;
        let mut files = Vec::new();
        let mut total_bytes = 0_u64;

        for index in 0..archive.len() {
            let mut file = archive.by_index(index)?;
            if file.is_dir() {
                continue;
            }
            if files.len() >= max_files || file.size() > max_file_bytes {
                return Err("asset archive exceeds configured limits".into());
            }

            let Some(path) = file.enclosed_name() else {
                continue;
            };
            let relative_path = sanitize_relative_path(&path.to_string_lossy());
            if relative_path.is_empty() {
                continue;
            }

            let mime_type = guess_mime_type(path.extension().and_then(|ext| ext.to_str()));
            let mut bytes = Vec::with_capacity(file.size() as usize);
            file.by_ref()
                .take(max_file_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max_file_bytes {
                return Err("asset archive file exceeds configured limit".into());
            }
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            if total_bytes > max_group_bytes {
                return Err("asset archive exceeds configured group limit".into());
            }
            files.push(AssetUploadFile {
                relative_path,
                bytes,
                mime_type,
            });
        }

        Ok(files)
    }

    pub async fn summarize_existing_group_files(
        &self,
        object_key: &str,
        relative_paths: &[String],
    ) -> Result<StoredAssetGroupSummary, RbInternalError> {
        let mut group_hasher = Sha256::new();
        let mut group_size = 0_u64;

        for relative_path in relative_paths {
            let file_path = self.object_path(object_key, relative_path);
            let bytes = fs::read(&file_path).await?;
            let file_size = bytes.len() as u64;
            group_hasher.update(relative_path.as_bytes());
            group_hasher.update([0]);
            group_hasher.update(file_size.to_le_bytes());
            group_hasher.update(&bytes);
            group_size += file_size;
        }

        Ok(StoredAssetGroupSummary {
            size: group_size,
            sha256: format!("{:x}", group_hasher.finalize()),
        })
    }

    pub async fn read_object_file_limited(
        &self,
        object_key: &str,
        relative_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, RbInternalError> {
        let file_path = self.object_path(object_key, relative_path);
        let metadata = fs::metadata(&file_path).await?;
        if metadata.len() > max_bytes {
            return Err(RbInternalError::Other(format!(
                "asset file is too large: {} bytes",
                metadata.len()
            )));
        }

        let mut file = fs::File::open(file_path).await?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes).await?;
        Ok(bytes)
    }
}

impl StorageManager {
    pub fn new(config: &StorageConfig) -> Result<Self, RbInternalError> {
        config.validate().map_err(RbInternalError::Other)?;
        let mut backends = HashMap::with_capacity(config.backends.len());
        for (id, backend) in &config.backends {
            let (label, backend) = match backend {
                StorageBackendConfig::Local { label, asset_root } => {
                    (label, StorageBackend::Local(LocalStorage::new(asset_root)))
                }
                StorageBackendConfig::Cos {
                    label,
                    region,
                    bucket,
                    secret_id,
                    secret_key,
                    public_base_url,
                } => (
                    label,
                    StorageBackend::Cos(CosStorage::new(
                        region,
                        bucket,
                        secret_id,
                        secret_key,
                        public_base_url,
                    )),
                ),
                StorageBackendConfig::Database { label } => (label, StorageBackend::Database),
            };
            backends.insert(
                id.clone(),
                StorageInstance {
                    label: label.clone().into(),
                    backend,
                },
            );
        }
        Ok(Self {
            backends: Arc::new(backends),
            default_backend: config.default_backend.clone().into(),
            database_cache: DatabaseAssetCache::new(),
        })
    }

    pub fn local(&self, backend: &str) -> Option<&LocalStorage> {
        match &self.backends.get(backend)?.backend {
            StorageBackend::Local(local) => Some(local),
            StorageBackend::Cos(_) => None,
            StorageBackend::Database => None,
        }
    }

    pub fn is_database(&self, backend: &str) -> bool {
        matches!(
            self.backends.get(backend).map(|instance| &instance.backend),
            Some(StorageBackend::Database)
        )
    }

    pub fn supports_public_read(&self, backend: &str) -> bool {
        matches!(
            self.backends.get(backend).map(|instance| &instance.backend),
            Some(StorageBackend::Local(_) | StorageBackend::Cos(_))
        )
    }

    pub fn cached_database_asset(&self, sha256: &str) -> Option<Arc<[u8]>> {
        self.database_cache.get(sha256)
    }

    pub fn cache_database_asset(&self, sha256: &str, content: Arc<[u8]>) {
        self.database_cache.insert(sha256.to_string(), content);
    }

    pub fn available_backends(&self) -> Vec<ConfiguredStorageBackend> {
        let mut backends = self
            .backends
            .iter()
            .map(|(id, instance)| ConfiguredStorageBackend {
                id: id.clone(),
                kind: instance.backend.kind(),
                label: instance.label.to_string(),
                recommended: id == self.default_backend.as_ref(),
                public_read: !matches!(instance.backend, StorageBackend::Database),
                backend_read: matches!(
                    instance.backend,
                    StorageBackend::Local(_) | StorageBackend::Database
                ),
                allowed_scopes: if matches!(instance.backend, StorageBackend::Database) {
                    &["game", "puzzle"]
                } else {
                    &["game", "puzzle", "round"]
                },
                max_file_bytes: matches!(instance.backend, StorageBackend::Database)
                    .then_some(DATABASE_MAX_FILE_BYTES),
                max_group_bytes: matches!(instance.backend, StorageBackend::Database)
                    .then_some(DATABASE_MAX_GROUP_BYTES),
            })
            .collect::<Vec<_>>();
        backends.sort_by(|a, b| {
            b.recommended
                .cmp(&a.recommended)
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.id.cmp(&b.id))
        });
        backends
    }

    pub fn has_backend(&self, backend: &str) -> bool {
        self.backends.contains_key(backend)
    }

    pub fn supports_public_url(&self, backend: &str) -> bool {
        matches!(
            self.backends.get(backend).map(|instance| &instance.backend),
            Some(StorageBackend::Cos(_))
        )
    }

    pub async fn store_group_files(
        &self,
        backend: &str,
        object_key: &str,
        files: &[AssetUploadFile],
    ) -> Result<StoredAssetGroup, RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => local.store_group_files(object_key, files).await,
            StorageBackend::Cos(cos) => cos.store_group_files(object_key, files).await,
            StorageBackend::Database => {
                Err("database assets require a database transaction".into())
            }
        }
    }

    pub async fn store_public_file(
        &self,
        backend: &str,
        object_key: &str,
        relative_path: &str,
        bytes: &[u8],
        mime_type: &str,
        cache_control: Option<&str>,
    ) -> Result<StoredPublicFile, RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => {
                local
                    .store_public_file(object_key, relative_path, bytes, mime_type, cache_control)
                    .await
            }
            StorageBackend::Cos(cos) => {
                cos.put(object_key, relative_path, bytes, mime_type, cache_control)
                    .await?;
                let mut hasher = Sha256::new();
                hasher.update(bytes);
                Ok(StoredPublicFile {
                    size: bytes.len() as u64,
                    sha256: format!("{:x}", hasher.finalize()),
                })
            }
            StorageBackend::Database => {
                Err("database content cannot be used as CDN storage".into())
            }
        }
    }

    pub async fn summarize_existing_group_files(
        &self,
        backend: &str,
        object_key: &str,
        relative_paths: &[String],
    ) -> Result<StoredAssetGroupSummary, RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => {
                local
                    .summarize_existing_group_files(object_key, relative_paths)
                    .await
            }
            StorageBackend::Cos(cos) => {
                cos.summarize_existing_group_files(object_key, relative_paths)
                    .await
            }
            StorageBackend::Database => {
                Err("database assets must be summarized transactionally".into())
            }
        }
    }

    pub async fn rename_file(
        &self,
        backend: &str,
        object_key: &str,
        old_path: &str,
        new_path: &str,
        mime_type: &str,
    ) -> Result<(), RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => {
                let old_path = local.object_path(object_key, old_path);
                let new_path = local.object_path(object_key, new_path);
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent).await?;
                }
                fs::rename(old_path, new_path).await?;
                Ok(())
            }
            StorageBackend::Cos(cos) => cos.rename(object_key, old_path, new_path, mime_type).await,
            StorageBackend::Database => Ok(()),
        }
    }

    pub async fn delete_files(
        &self,
        backend: &str,
        object_key: &str,
        relative_paths: &[String],
    ) -> Result<(), RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => {
                if relative_paths.len() == 1 {
                    fs::remove_file(local.object_path(object_key, &relative_paths[0])).await?;
                } else {
                    fs::remove_dir_all(local.object_dir(object_key)).await?;
                }
                Ok(())
            }
            StorageBackend::Cos(cos) => {
                for path in relative_paths {
                    cos.delete(object_key, path).await?;
                }
                Ok(())
            }
            StorageBackend::Database => Ok(()),
        }
    }

    pub fn public_url(
        &self,
        backend: &str,
        object_key: &str,
        relative_path: &str,
    ) -> Option<String> {
        match &self.backends.get(backend)?.backend {
            StorageBackend::Local(_) => None,
            StorageBackend::Cos(cos) => Some(cos.public_url(object_key, relative_path)),
            StorageBackend::Database => None,
        }
    }

    pub fn asset_public_url(
        &self,
        backend: &str,
        object_key: &str,
        relative_path: &str,
    ) -> Option<String> {
        match &self.backends.get(backend)?.backend {
            StorageBackend::Local(_) => Some(build_public_path(object_key, relative_path)),
            StorageBackend::Cos(cos) => Some(cos.public_url(object_key, relative_path)),
            StorageBackend::Database => None,
        }
    }

    pub fn asset_group_public_url(&self, backend: &str, object_key: &str) -> Option<String> {
        match &self.backends.get(backend)?.backend {
            StorageBackend::Local(_) => Some(build_public_group_path(object_key)),
            StorageBackend::Cos(cos) => Some(cos.group_public_url(object_key)),
            StorageBackend::Database => None,
        }
    }

    pub async fn ready(&self) -> bool {
        for instance in self.backends.values() {
            let ready = match &instance.backend {
                StorageBackend::Local(local) => fs::metadata(local.root())
                    .await
                    .is_ok_and(|metadata| metadata.is_dir()),
                StorageBackend::Cos(cos) => cos.ready().await,
                StorageBackend::Database => true,
            };
            if !ready {
                return false;
            }
        }
        true
    }

    fn backend(&self, backend: &str) -> Result<&StorageInstance, RbInternalError> {
        self.backends
            .get(backend)
            .ok_or_else(|| RbInternalError::Other(format!("unknown storage backend: {backend}")))
    }
}

impl StorageBackend {
    fn kind(&self) -> &'static str {
        match self {
            Self::Local(_) => "local",
            Self::Cos(_) => "cos",
            Self::Database => "database",
        }
    }
}

impl DatabaseAssetCache {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DatabaseAssetCacheInner {
                entries: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
            })),
        }
    }

    fn get(&self, key: &str) -> Option<Arc<[u8]>> {
        let mut cache = self.inner.lock().ok()?;
        cache.remove_expired();
        let content = {
            let entry = cache.entries.get_mut(key)?;
            entry.last_used = Instant::now();
            entry.content.clone()
        };
        cache.touch(key);
        Some(content)
    }

    fn insert(&self, key: String, content: Arc<[u8]>) {
        let size = content.len() as u64;
        if size > DATABASE_CACHE_BYTES {
            return;
        }
        let Ok(mut cache) = self.inner.lock() else {
            return;
        };
        cache.remove_expired();
        if let Some(previous) = cache.entries.remove(&key) {
            cache.bytes = cache.bytes.saturating_sub(previous.content.len() as u64);
        }
        cache.order.retain(|current| current != &key);
        while cache.bytes.saturating_add(size) > DATABASE_CACHE_BYTES {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            if let Some(entry) = cache.entries.remove(&oldest) {
                cache.bytes = cache.bytes.saturating_sub(entry.content.len() as u64);
            }
        }
        cache.bytes = cache.bytes.saturating_add(size);
        cache.order.push_back(key.clone());
        cache.entries.insert(
            key,
            DatabaseAssetCacheEntry {
                content,
                last_used: Instant::now(),
            },
        );
    }
}

impl DatabaseAssetCacheInner {
    fn touch(&mut self, key: &str) {
        self.order.retain(|current| current != key);
        self.order.push_back(key.to_string());
    }

    fn remove_expired(&mut self) {
        let now = Instant::now();
        let expired = self
            .entries
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.last_used) >= DATABASE_CACHE_IDLE)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in expired {
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(entry.content.len() as u64);
            }
            self.order.retain(|current| current != &key);
        }
    }
}

impl CosStorage {
    fn new(
        region: &str,
        bucket: &str,
        secret_id: &str,
        secret_key: &str,
        public_base_url: &str,
    ) -> Self {
        Self {
            client: Client::new(),
            host: format!("{bucket}.cos.{region}.myqcloud.com").into(),
            secret_id: secret_id.to_string().into(),
            secret_key: secret_key.to_string().into(),
            public_base_url: public_base_url.trim_end_matches('/').to_string().into(),
        }
    }

    async fn store_group_files(
        &self,
        object_key: &str,
        files: &[AssetUploadFile],
    ) -> Result<StoredAssetGroup, RbInternalError> {
        let mut stored_files: Vec<StoredAssetFile> = Vec::with_capacity(files.len());
        let mut used_paths = HashMap::new();
        let mut group_hasher = Sha256::new();
        let mut group_size = 0_u64;

        for file in files {
            let relative_path = uniquify_relative_path(
                sanitize_relative_path(&file.relative_path),
                &mut used_paths,
            );
            if let Err(error) = self
                .put(
                    object_key,
                    &relative_path,
                    &file.bytes,
                    &file.mime_type,
                    None,
                )
                .await
            {
                for stored in &stored_files {
                    let _ = self.delete(object_key, &stored.relative_path).await;
                }
                return Err(error);
            }

            let mut hasher = Sha256::new();
            hasher.update(&file.bytes);
            let file_size = file.bytes.len() as u64;
            group_hasher.update(relative_path.as_bytes());
            group_hasher.update([0]);
            group_hasher.update(file_size.to_le_bytes());
            group_hasher.update(&file.bytes);
            group_size += file_size;
            stored_files.push(StoredAssetFile {
                relative_path: relative_path.clone(),
                size: file_size,
                sha256: format!("{:x}", hasher.finalize()),
                mime_type: file.mime_type.clone(),
                path: build_public_path(object_key, &relative_path),
            });
        }

        Ok(StoredAssetGroup {
            size: group_size,
            sha256: format!("{:x}", group_hasher.finalize()),
            files: stored_files,
        })
    }

    async fn summarize_existing_group_files(
        &self,
        object_key: &str,
        relative_paths: &[String],
    ) -> Result<StoredAssetGroupSummary, RbInternalError> {
        let mut group_hasher = Sha256::new();
        let mut group_size = 0_u64;
        for relative_path in relative_paths {
            let bytes = self.get(object_key, relative_path).await?;
            let file_size = bytes.len() as u64;
            group_hasher.update(relative_path.as_bytes());
            group_hasher.update([0]);
            group_hasher.update(file_size.to_le_bytes());
            group_hasher.update(&bytes);
            group_size += file_size;
        }
        Ok(StoredAssetGroupSummary {
            size: group_size,
            sha256: format!("{:x}", group_hasher.finalize()),
        })
    }

    async fn rename(
        &self,
        object_key: &str,
        old_path: &str,
        new_path: &str,
        _mime_type: &str,
    ) -> Result<(), RbInternalError> {
        self.copy(object_key, old_path, new_path).await?;
        if let Err(error) = self.delete(object_key, old_path).await {
            let _ = self.delete(object_key, new_path).await;
            return Err(error);
        }
        Ok(())
    }

    async fn put(
        &self,
        object_key: &str,
        relative_path: &str,
        bytes: &[u8],
        mime_type: &str,
        cache_control: Option<&str>,
    ) -> Result<(), RbInternalError> {
        let path = cos_object_path(object_key, relative_path);
        let mut request = self
            .signed_request(Method::PUT, &path, &[])
            .header(header::CONTENT_TYPE, mime_type)
            .body(bytes.to_vec());
        if let Some(cache_control) = cache_control {
            request = request.header(header::CACHE_CONTROL, cache_control);
        }
        let response = request.send().await.map_err(storage_http_error)?;
        ensure_cos_success(response).await
    }

    async fn get(&self, object_key: &str, relative_path: &str) -> Result<Vec<u8>, RbInternalError> {
        let path = cos_object_path(object_key, relative_path);
        let response = self
            .signed_request(Method::GET, &path, &[])
            .send()
            .await
            .map_err(storage_http_error)?;
        if !response.status().is_success() {
            return Err(cos_response_error(response).await);
        }
        Ok(response.bytes().await.map_err(storage_http_error)?.to_vec())
    }

    async fn delete(&self, object_key: &str, relative_path: &str) -> Result<(), RbInternalError> {
        let path = cos_object_path(object_key, relative_path);
        let response = self
            .signed_request(Method::DELETE, &path, &[])
            .send()
            .await
            .map_err(storage_http_error)?;
        ensure_cos_success(response).await
    }

    async fn ready(&self) -> bool {
        self.signed_request(Method::HEAD, "/", &[])
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    fn public_url(&self, object_key: &str, relative_path: &str) -> String {
        format!(
            "{}{}",
            self.public_base_url,
            cos_object_path(object_key, relative_path)
        )
    }

    fn group_public_url(&self, object_key: &str) -> String {
        format!("{}{}/", self.public_base_url, cos_group_path(object_key))
    }

    async fn copy(
        &self,
        object_key: &str,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), RbInternalError> {
        let source_path = cos_object_path(object_key, old_path);
        let target_path = cos_object_path(object_key, new_path);
        let copy_source = format!("{}{}", self.host, source_path);
        let response = self
            .signed_request(
                Method::PUT,
                &target_path,
                &[("x-cos-copy-source", &copy_source)],
            )
            .send()
            .await
            .map_err(storage_http_error)?;
        if !response.status().is_success() {
            return Err(cos_response_error(response).await);
        }
        let body = response.text().await.map_err(storage_http_error)?;
        if body.contains("<Error>") {
            return Err(RbInternalError::Other(format!(
                "COS copy request failed: {body}"
            )));
        }
        Ok(())
    }

    fn signed_request(
        &self,
        method: Method,
        path: &str,
        extra_headers: &[(&str, &str)],
    ) -> reqwest::RequestBuilder {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let key_time = format!("{now};{}", now + 900);
        let mut headers = vec![("host", self.host.as_ref())];
        headers.extend_from_slice(extra_headers);
        headers.sort_unstable_by_key(|(name, _)| *name);
        let header_list = headers
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(";");
        let canonical_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}={}", encode_cos_component(value)))
            .collect::<Vec<_>>()
            .join("&");
        let http_string = format!(
            "{}\n{}\n\n{}\n",
            method.as_str().to_ascii_lowercase(),
            path,
            canonical_headers
        );
        let string_to_sign = format!("sha1\n{key_time}\n{}\n", sha1_hex(http_string.as_bytes()));
        let sign_key = hmac_sha1_hex(self.secret_key.as_bytes(), key_time.as_bytes());
        let signature = hmac_sha1_hex(sign_key.as_bytes(), string_to_sign.as_bytes());
        let authorization = format!(
            "q-sign-algorithm=sha1&q-ak={}&q-sign-time={key_time}&q-key-time={key_time}&q-header-list={header_list}&q-url-param-list=&q-signature={signature}",
            self.secret_id
        );

        let mut request = self
            .client
            .request(method, format!("https://{}{}", self.host, path))
            .header(header::AUTHORIZATION, authorization);
        for (name, value) in extra_headers {
            request = request.header(*name, *value);
        }
        request
    }
}

async fn ensure_cos_success(response: reqwest::Response) -> Result<(), RbInternalError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(cos_response_error(response).await)
    }
}

async fn cos_response_error(response: reqwest::Response) -> RbInternalError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    RbInternalError::Other(format!("COS request failed with {status}: {body}"))
}

fn storage_http_error(error: reqwest::Error) -> RbInternalError {
    RbInternalError::Other(format!("storage request failed: {error}"))
}

fn cos_object_path(object_key: &str, relative_path: &str) -> String {
    let path = format!(
        "{}/{}",
        sanitize_object_key(object_key),
        sanitize_relative_path(relative_path)
    );
    format!(
        "/{}",
        path.split('/')
            .map(|segment| utf8_percent_encode(segment, URL_PATH_SEGMENT_ENCODE_SET).to_string())
            .collect::<Vec<_>>()
            .join("/")
    )
}

fn cos_group_path(object_key: &str) -> String {
    format!(
        "/{}",
        utf8_percent_encode(
            &sanitize_object_key(object_key),
            URL_PATH_SEGMENT_ENCODE_SET
        )
    )
}

fn encode_cos_component(value: &str) -> String {
    utf8_percent_encode(value, URL_PATH_SEGMENT_ENCODE_SET).to_string()
}

fn sha1_hex(value: &[u8]) -> String {
    use sha1::Digest as _;
    format!("{:x}", Sha1::digest(value))
}

fn hmac_sha1_hex(key: &[u8], value: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts arbitrary key lengths");
    mac.update(value);
    format!("{:x}", mac.finalize().into_bytes())
}

pub fn build_public_path(object_key: &str, original_name: &str) -> String {
    let encoded_object_key =
        utf8_percent_encode(object_key, URL_PATH_SEGMENT_ENCODE_SET).to_string();
    let safe_name = sanitize_relative_path(original_name);
    let encoded_name = safe_name
        .split('/')
        .map(|segment| utf8_percent_encode(segment, URL_PATH_SEGMENT_ENCODE_SET).to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("/assets/{encoded_object_key}/{encoded_name}")
}

pub fn build_public_group_path(object_key: &str) -> String {
    let encoded_object_key =
        utf8_percent_encode(object_key, URL_PATH_SEGMENT_ENCODE_SET).to_string();
    format!("/assets/{encoded_object_key}/")
}

fn sanitize_object_key(value: &str) -> String {
    if value.is_empty() {
        "invalid-object-key".to_string()
    } else {
        utf8_percent_encode(value, STORAGE_OBJECT_KEY_ENCODE_SET).to_string()
    }
}

pub fn sanitize_relative_path(value: &str) -> String {
    let mut parts = Vec::new();

    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => {
                let part = sanitize_filename(part.to_string_lossy().as_ref());
                if !part.is_empty() {
                    parts.push(part);
                }
            }
            Component::CurDir => {}
            _ => {}
        }
    }

    if parts.is_empty() {
        "file".to_string()
    } else {
        parts.join("/")
    }
}

fn sanitize_filename(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch != '\0' && ch != '/' && ch != '\\' {
            result.push(ch);
        }
    }
    if result.is_empty() {
        "file".to_string()
    } else {
        result
    }
}

fn guess_mime_type(ext: Option<&str>) -> String {
    match ext.unwrap_or_default().to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

pub fn uniquify_relative_path(path: String, used_paths: &mut HashMap<String, usize>) -> String {
    let count = used_paths.entry(path.clone()).or_insert(0);
    if *count == 0 {
        *count = 1;
        return path;
    }

    let mut current = *count;
    *count += 1;

    let (dir, file) = match path.rsplit_once('/') {
        Some((dir, file)) => (Some(dir.to_string()), file.to_string()),
        None => (None, path),
    };
    let (stem, ext) = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            (stem.to_string(), Some(ext.to_string()))
        }
        _ => (file, None),
    };

    loop {
        let candidate_file = match &ext {
            Some(ext) => format!("{stem}_{current}.{ext}"),
            None => format!("{stem}_{current}"),
        };
        let candidate = match &dir {
            Some(dir) => format!("{dir}/{candidate_file}"),
            None => candidate_file,
        };
        if !used_paths.contains_key(&candidate) {
            used_paths.insert(candidate.clone(), 1);
            return candidate;
        }
        current += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Write},
        path::{Component, Path},
        sync::Arc,
    };

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::{
        CosStorage, DatabaseAssetCache, LocalStorage, build_public_group_path, build_public_path,
        cos_object_path,
    };

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (path, content) in files {
            writer
                .start_file(*path, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn public_path_encodes_literal_percent_signs() {
        assert_eq!(
            build_public_path(
                "group-test",
                "%7BC5F0FF2C-5587-4b4a-876D-2431FA496E48%7D.png"
            ),
            "/assets/group-test/%257BC5F0FF2C-5587-4b4a-876D-2431FA496E48%257D.png"
        );
    }

    #[test]
    fn public_path_encodes_each_path_segment() {
        assert_eq!(
            build_public_path("group-test", "目录/a b#c.png"),
            "/assets/group-test/%E7%9B%AE%E5%BD%95/a%20b%23c.png"
        );
    }

    #[test]
    fn public_group_paths_have_a_trailing_slash() {
        assert_eq!(build_public_group_path("group-test"), "/assets/group-test/");

        let cos = CosStorage::new(
            "ap-test",
            "bucket",
            "secret-id",
            "secret-key",
            "https://assets.example.com/",
        );
        assert_eq!(
            cos.group_public_url("group-test"),
            "https://assets.example.com/group-test/"
        );
    }

    #[test]
    fn cos_object_path_encodes_each_path_segment() {
        assert_eq!(
            cos_object_path("group-test", "目录/a b#c.png"),
            "/group-test/%E7%9B%AE%E5%BD%95/a%20b%23c.png"
        );
    }

    #[test]
    fn object_keys_cannot_escape_the_storage_root() {
        let storage = LocalStorage::new("/tmp/rbph-assets");
        let object_dir = storage.object_dir("../outside");

        assert!(object_dir.starts_with(Path::new("/tmp/rbph-assets")));
        assert_eq!(object_dir, Path::new("/tmp/rbph-assets/%2E%2E%2Foutside"));
    }

    #[test]
    fn untrusted_paths_remain_inside_the_storage_root() {
        let root = Path::new("/tmp/rbph-assets");
        let storage = LocalStorage::new(root);
        let object_keys = [
            "..",
            "../outside",
            "/absolute",
            "a/b",
            r"a\b",
            "%2e%2e",
            "group-safe",
        ];
        let relative_paths = [
            "..",
            "../secret",
            "../../etc/passwd",
            "/etc/passwd",
            r"..\..\secret",
            "a/../../../secret",
            "%2e%2e/%2fetc",
            "safe/file.png",
        ];

        for object_key in object_keys {
            for relative_path in relative_paths {
                let path = storage.object_path(object_key, relative_path);
                let remainder = path
                    .strip_prefix(root)
                    .expect("asset path must remain under its storage root");
                assert!(
                    remainder
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))),
                    "unsafe path for object_key={object_key:?}, relative_path={relative_path:?}: {path:?}"
                );
            }
        }
    }

    #[test]
    fn zip_extraction_enforces_each_limit() {
        let bytes = archive(&[("a.txt", b"1234"), ("b.txt", b"5678")]);
        assert!(LocalStorage::unpack_zip_files_limited(&bytes, 4, 8, 2).is_ok());
        assert!(LocalStorage::unpack_zip_files_limited(&bytes, 3, 8, 2).is_err());
        assert!(LocalStorage::unpack_zip_files_limited(&bytes, 4, 7, 2).is_err());
        assert!(LocalStorage::unpack_zip_files_limited(&bytes, 4, 8, 1).is_err());
    }

    #[test]
    fn database_cache_returns_inserted_content() {
        let cache = DatabaseAssetCache::new();
        let content: Arc<[u8]> = Arc::from(b"hidden".as_slice());
        cache.insert("sha".to_string(), content.clone());
        assert_eq!(cache.get("sha").unwrap().as_ref(), content.as_ref());
        assert!(cache.get("missing").is_none());
    }
}

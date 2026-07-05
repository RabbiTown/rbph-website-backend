use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
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

#[derive(Clone)]
pub struct LocalStorage {
    root: Arc<PathBuf>,
}

#[derive(Clone)]
pub struct StorageManager {
    backends: Arc<HashMap<String, StorageInstance>>,
    default_backend: Arc<str>,
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
}

pub struct ConfiguredStorageBackend {
    pub id: String,
    pub kind: &'static str,
    pub label: String,
    pub recommended: bool,
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

    pub fn unpack_zip_files(bytes: &[u8]) -> Result<Vec<AssetUploadFile>, RbInternalError> {
        let cursor = Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor)?;
        let mut files = Vec::new();

        for index in 0..archive.len() {
            let mut file = archive.by_index(index)?;
            if file.is_dir() {
                continue;
            }

            let Some(path) = file.enclosed_name() else {
                continue;
            };
            let relative_path = sanitize_relative_path(&path.to_string_lossy());
            if relative_path.is_empty() {
                continue;
            }

            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            files.push(AssetUploadFile {
                relative_path,
                bytes,
                mime_type: guess_mime_type(path.extension().and_then(|ext| ext.to_str())),
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
        })
    }

    pub fn local(&self, backend: &str) -> Option<&LocalStorage> {
        match &self.backends.get(backend)?.backend {
            StorageBackend::Local(local) => Some(local),
            StorageBackend::Cos(_) => None,
        }
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

    pub async fn store_group_files(
        &self,
        backend: &str,
        object_key: &str,
        files: &[AssetUploadFile],
    ) -> Result<StoredAssetGroup, RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Local(local) => local.store_group_files(object_key, files).await,
            StorageBackend::Cos(cos) => cos.store_group_files(object_key, files).await,
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
        }
    }

    pub async fn ready(&self) -> bool {
        for instance in self.backends.values() {
            let ready = match &instance.backend {
                StorageBackend::Local(local) => fs::metadata(local.root())
                    .await
                    .is_ok_and(|metadata| metadata.is_dir()),
                StorageBackend::Cos(cos) => cos.ready().await,
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
                .put(object_key, &relative_path, &file.bytes, &file.mime_type)
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
    ) -> Result<(), RbInternalError> {
        let path = cos_object_path(object_key, relative_path);
        let response = self
            .signed_request(Method::PUT, &path, &[])
            .header(header::CONTENT_TYPE, mime_type)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(storage_http_error)?;
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

fn uniquify_relative_path(path: String, used_paths: &mut HashMap<String, usize>) -> String {
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
    use std::path::{Component, Path};

    use super::{LocalStorage, build_public_path, cos_object_path};

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
}

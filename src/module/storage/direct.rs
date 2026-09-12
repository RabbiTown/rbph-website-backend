//! COS control requests. File bodies are uploaded by the browser, never proxied here.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Part {
    pub part_number: u32,
    #[serde(rename = "ETag")]
    pub etag: String,
    pub size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Initiated {
    upload_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Parts {
    #[serde(default, rename = "Part")]
    parts: Vec<Part>,
    #[serde(default)]
    is_truncated: bool,
    next_part_number_marker: Option<u32>,
}

#[derive(Deserialize)]
struct Completed {
    #[serde(rename = "ETag")]
    etag: String,
}

#[derive(Serialize)]
pub struct PartAuthorization {
    pub url: String,
    pub authorization: String,
    pub content_md5: String,
}

pub struct ObjectHead {
    pub size: u64,
    pub session: Option<String>,
}

impl StorageManager {
    pub(crate) fn cos(&self, backend: &str) -> Result<&CosStorage, RbInternalError> {
        match &self.backend(backend)?.backend {
            StorageBackend::Cos(cos) => Ok(cos),
            _ => Err("direct upload requires COS".into()),
        }
    }
}

fn canonical(values: &[(&str, &str)]) -> (String, String) {
    let mut values = values
        .iter()
        .map(|(k, v)| {
            (
                encode_cos_component(&k.to_ascii_lowercase()),
                encode_cos_component(v),
            )
        })
        .collect::<Vec<_>>();
    values.sort_unstable();
    (
        values
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(";"),
        values
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&"),
    )
}

fn sign(
    secret_id: &str,
    secret_key: &str,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    validity: (u64, u64),
) -> String {
    let (now, expires) = validity;
    let key_time = format!("{now};{expires}");

    let (header_list, canonical_headers) = canonical(headers);
    let (query_list, canonical_query) = canonical(query);

    // COS signs the decoded URI; the HTTP request still uses the encoded path.
    let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
    let http_string = format!(
        "{}\n{path}\n{canonical_query}\n{canonical_headers}\n",
        method.to_ascii_lowercase()
    );

    let string_to_sign = format!("sha1\n{key_time}\n{}\n", sha1_hex(http_string.as_bytes()));
    let sign_key = hmac_sha1_hex(secret_key.as_bytes(), key_time.as_bytes());

    let signature = hmac_sha1_hex(sign_key.as_bytes(), string_to_sign.as_bytes());
    format!(
        "q-sign-algorithm=sha1&q-ak={secret_id}&q-sign-time={key_time}&q-key-time={key_time}&q-header-list={header_list}&q-url-param-list={query_list}&q-signature={signature}"
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn xml<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, RbInternalError> {
    quick_xml::de::from_str(body).map_err(|_| "invalid COS control response".into())
}

async fn control_text(response: reqwest::Response) -> Result<String, RbInternalError> {
    if !response.status().is_success() {
        return Err(cos_response_error(response).await);
    }

    // Control responses are bounded, unlike uploaded content.
    let mut response = response;
    let mut bytes = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?
    {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err("COS control response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }

    String::from_utf8(bytes).map_err(|_| "invalid COS response encoding".into())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PendingUploads {
    #[serde(default, rename = "Upload")]
    uploads: Vec<PendingUpload>,
    #[serde(default)]
    is_truncated: bool,
    next_key_marker: Option<String>,
    next_upload_id_marker: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PendingUpload {
    key: String,
    upload_id: String,
}

impl CosStorage {
    pub(super) fn control(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
    ) -> reqwest::RequestBuilder {
        let mut headers = vec![("host", self.host.as_ref())];
        headers.extend_from_slice(extra);

        let auth = sign(
            &self.secret_id,
            &self.secret_key,
            method.as_str(),
            path,
            query,
            &headers,
            (now(), now() + 900),
        );

        let url = format!("{}{path}", self.control_endpoint);
        let mut request = self
            .client
            .request(method, url)
            .query(query)
            .header("Authorization", auth)
            .timeout(std::time::Duration::from_secs(600));

        for (k, v) in extra {
            request = request.header(*k, *v);
        }
        request
    }

    /// Recover even an initiation whose response was lost before upload_id was saved.
    pub(crate) async fn pending(
        &self,
        key: &str,
        path: &str,
    ) -> Result<Vec<String>, RbInternalError> {
        let object = format!("{key}/{path}");
        let mut found = Vec::new();

        let mut marker = String::new();
        let mut upload_marker = String::new();

        loop {
            let response = self
                .control(
                    Method::GET,
                    "/",
                    &[
                        ("uploads", ""),
                        ("prefix", &object),
                        ("key-marker", &marker),
                        ("upload-id-marker", &upload_marker),
                    ],
                    &[],
                )
                .send()
                .await
                .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;

            let page: PendingUploads = xml(&control_text(response).await?)?;
            found.extend(
                page.uploads
                    .into_iter()
                    .filter(|u| u.key == object)
                    .map(|u| u.upload_id),
            );
            if !page.is_truncated {
                return Ok(found);
            }

            let next = (
                page.next_key_marker.unwrap_or_default(),
                page.next_upload_id_marker.unwrap_or_default(),
            );
            if next == (marker.clone(), upload_marker.clone()) {
                return Err("invalid COS upload pagination".into());
            }
            (marker, upload_marker) = next;
        }
    }

    pub(crate) async fn initiate(
        &self,
        key: &str,
        path: &str,
        mime: &str,
        session: &str,
    ) -> Result<String, RbInternalError> {
        let response = self
            .control(
                Method::POST,
                &cos_object_path(key, path),
                &[("uploads", "")],
                &[
                    ("content-type", mime),
                    ("x-cos-meta-upload-session", session),
                ],
            )
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;

        let result: Initiated = xml(&control_text(response).await?)?;
        if result.upload_id.is_empty() {
            return Err("COS returned empty upload id".into());
        }

        Ok(result.upload_id)
    }

    pub(crate) fn authorize_part(
        &self,
        key: &str,
        path: &str,
        upload: &str,
        part: u32,
        content: (&str, u64),
        expires: u64,
    ) -> PartAuthorization {
        let path = cos_object_path(key, path);
        let number = part.to_string();
        let (md5, size) = content;
        let length = size.to_string();

        let query = [("partNumber", number.as_str()), ("uploadId", upload)];
        let authorization = sign(
            &self.secret_id,
            &self.secret_key,
            "PUT",
            &path,
            &query,
            &[
                ("host", self.host.as_ref()),
                ("content-md5", md5),
                ("content-length", &length),
            ],
            (now(), expires),
        );

        let url = format!(
            "{}{path}?partNumber={part}&uploadId={}",
            self.control_endpoint,
            encode_cos_component(upload)
        );
        PartAuthorization {
            url,
            authorization,
            content_md5: md5.into(),
        }
    }

    pub(crate) async fn parts(
        &self,
        key: &str,
        path: &str,
        upload: &str,
    ) -> Result<Vec<Part>, RbInternalError> {
        let mut all = Vec::new();
        let mut marker = 0;

        loop {
            let marker_text = marker.to_string();
            let response = self
                .control(
                    Method::GET,
                    &cos_object_path(key, path),
                    &[("uploadId", upload), ("part-number-marker", &marker_text)],
                    &[],
                )
                .send()
                .await
                .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;

            let page: Parts = xml(&control_text(response).await?)?;
            all.extend(page.parts);
            if !page.is_truncated {
                return Ok(all);
            }

            let next = page
                .next_part_number_marker
                .ok_or_else(|| RbInternalError::from("COS missing pagination marker"))?;
            if next <= marker || all.len() > 10000 {
                return Err("invalid COS part pagination".into());
            }
            marker = next;
        }
    }

    pub(crate) async fn finish(
        &self,
        key: &str,
        path: &str,
        upload: &str,
        parts: &[Part],
    ) -> Result<(), RbInternalError> {
        let mut body = String::from("<CompleteMultipartUpload>");

        for part in parts {
            body.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                part.part_number,
                quick_xml::escape::escape(&part.etag)
            ));
        }
        body.push_str("</CompleteMultipartUpload>");

        let response = self
            .control(
                Method::POST,
                &cos_object_path(key, path),
                &[("uploadId", upload)],
                &[("content-type", "application/xml")],
            )
            .body(body)
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;

        let result: Completed = xml(&control_text(response).await?)?;
        if result.etag.is_empty() {
            return Err("COS did not complete upload".into());
        }

        Ok(())
    }

    pub(crate) async fn head(
        &self,
        key: &str,
        path: &str,
    ) -> Result<Option<ObjectHead>, RbInternalError> {
        let response = self
            .control(Method::HEAD, &cos_object_path(key, path), &[], &[])
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(cos_response_error(response).await);
        }

        let size = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| RbInternalError::from("COS missing size"))?;

        Ok(Some(ObjectHead {
            size,
            session: response
                .headers()
                .get("x-cos-meta-upload-session")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        }))
    }

    pub(crate) async fn empty(
        &self,
        key: &str,
        path: &str,
        mime: &str,
        session: &str,
    ) -> Result<(), RbInternalError> {
        let r = self
            .control(
                Method::PUT,
                &cos_object_path(key, path),
                &[],
                &[
                    ("content-type", mime),
                    ("x-cos-meta-upload-session", session),
                ],
            )
            .body(Vec::new())
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;
        ensure_cos_success(r).await
    }

    pub(crate) async fn abort(
        &self,
        key: &str,
        path: &str,
        upload: &str,
    ) -> Result<(), RbInternalError> {
        let r = self
            .control(
                Method::DELETE,
                &cos_object_path(key, path),
                &[("uploadId", upload)],
                &[],
            )
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        ensure_cos_success(r).await
    }

    /// Hash the actual object stream without retaining file contents in memory.
    /// A client digest is only a comparison value, never the stored authority.
    pub(crate) async fn verified_sha256(
        &self,
        key: &str,
        path: &str,
        expected_size: u64,
        session: &str,
        claimed: &str,
    ) -> Result<String, RbInternalError> {
        let mut response = self
            .control(Method::GET, &cos_object_path(key, path), &[], &[])
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(cos_response_error(response).await);
        }

        let headers = response.headers();
        if headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            != Some(expected_size)
            || headers
                .get("x-cos-meta-upload-session")
                .and_then(|v| v.to_str().ok())
                != Some(session)
        {
            return Err("COS object size or upload session changed during verification".into());
        }

        let mut hasher = Sha256::new();
        let mut size = 0u64;

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?
        {
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or("COS object size overflow")?;
            if size > expected_size {
                return Err("COS object exceeds declared size".into());
            }
            hasher.update(&chunk);
        }
        if size != expected_size {
            return Err("COS object stream was truncated".into());
        }

        let actual = format!("{:x}", hasher.finalize());
        if actual != claimed {
            return Err("file SHA-256 does not match the COS object".into());
        }

        Ok(actual)
    }

    pub(crate) async fn metadata(&self, key: &str, path: &str) -> Result<Vec<u8>, RbInternalError> {
        let mut response = self
            .control(Method::GET, &cos_object_path(key, path), &[], &[])
            .send()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(cos_response_error(response).await);
        }

        let mut bytes = Vec::new();

        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| RbInternalError::Other(e.without_url().to_string()))?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("theme metadata exceeds 1 MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }

        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_cos_request() {
        let signature = sign(
            "id",
            "secret",
            "PUT",
            "/group/%E7%9B%AE.txt",
            &[("uploadId", "a+b"), ("partNumber", "1")],
            &[("host", "example"), ("content-md5", "abc=")],
            (10, 20),
        );
        assert!(signature.ends_with("q-signature=2fb8bfc3cde256a2c6201ee51e2bb87e39889bb9"));
        assert!(signature.contains("q-url-param-list=partnumber;uploadid"));
        assert!(signature.contains("q-header-list=content-md5;host"));
    }

    #[test]
    fn part_authorization_binds_the_expected_length() {
        let cos = CosStorage::new(
            "test",
            "bucket",
            "id",
            "secret",
            "https://assets.example.com",
        );
        let auth = cos.authorize_part("group-test", "a.txt", "upload", 1, ("abc=", 8), u64::MAX);
        assert!(
            auth.authorization
                .contains("q-header-list=content-length;content-md5;host")
        );
        let start = auth
            .authorization
            .split('&')
            .find_map(|field| field.strip_prefix("q-sign-time="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let expected = sign(
            "id",
            "secret",
            "PUT",
            "/group-test/a.txt",
            &[("partNumber", "1"), ("uploadId", "upload")],
            &[
                ("host", cos.host.as_ref()),
                ("content-md5", "abc="),
                ("content-length", "8"),
            ],
            (start, u64::MAX),
        );

        assert_eq!(auth.authorization, expected);
    }

    async fn mock(responses: Vec<String>) -> CosStorage {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 32768];

                let _ = stream.read(&mut buffer).await.unwrap();
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let mut cos = CosStorage::new(
            "test",
            "bucket",
            "id",
            "secret",
            "https://assets.example.com",
        );
        cos.control_endpoint = format!("http://{address}").into();
        cos.host = address.to_string().into();
        cos
    }

    fn response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    #[tokio::test]
    async fn lists_all_parts_across_pages() {
        let cos=mock(vec![
            response("<ListPartsResult><IsTruncated>true</IsTruncated><NextPartNumberMarker>1</NextPartNumberMarker><Part><PartNumber>1</PartNumber><ETag>a</ETag><Size>8388608</Size></Part></ListPartsResult>"),
            response("<ListPartsResult><IsTruncated>false</IsTruncated><Part><PartNumber>2</PartNumber><ETag>b</ETag><Size>1</Size></Part></ListPartsResult>"),
        ]).await;

        let parts = cos
            .parts("group-test", "foo/a.txt", "upload-id")
            .await
            .unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].size, 1);
    }

    #[tokio::test]
    async fn recovers_pending_upload_and_object_metadata() {
        let cos=mock(vec![
            response("<ListMultipartUploadsResult><IsTruncated>false</IsTruncated><Upload><Key>group-test/a</Key><UploadId>correct</UploadId></Upload></ListMultipartUploadsResult>"),
            "HTTP/1.1 200 OK\r\nContent-Length: 123\r\nx-cos-meta-upload-session: session-1\r\nConnection: close\r\n\r\n".into(),
        ]).await;
        assert_eq!(
            cos.pending("group-test", "a").await.unwrap(),
            vec!["correct"]
        );

        let head = cos.head("group-test", "a").await.unwrap().unwrap();
        assert_eq!(head.size, 123);
        assert_eq!(head.session.as_deref(), Some("session-1"));
    }

    #[tokio::test]
    async fn fingerprints_cos_object_contents() {
        let body = "actual object contents";
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));

        let object = || {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nx-cos-meta-upload-session: session-1\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        };

        let cos = mock(vec![object(), object(), object(), object()]).await;
        assert_eq!(
            cos.verified_sha256("group", "file", body.len() as u64, "session-1", &digest)
                .await
                .unwrap(),
            digest
        );

        assert!(
            cos.verified_sha256(
                "group",
                "file",
                body.len() as u64,
                "session-1",
                &"0".repeat(64)
            )
            .await
            .is_err()
        );
        assert!(
            cos.verified_sha256("group", "file", body.len() as u64 + 1, "session-1", &digest)
                .await
                .is_err()
        );
        assert!(
            cos.verified_sha256("group", "file", body.len() as u64, "wrong-session", &digest)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn rejects_cos_completion_error_and_truncated_content() {
        let cos = mock(vec![
            response("<Error><Code>InvalidPart</Code></Error>"),
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nx-cos-meta-upload-session: s\r\nConnection: close\r\n\r\nshort".into(),
        ]).await;

        assert!(cos.finish("group", "file", "upload", &[]).await.is_err());
        assert!(
            cos.verified_sha256("group", "file", 9, "s", &"0".repeat(64))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fingerprints_empty_cos_object() {
        let cos = mock(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nx-cos-meta-upload-session: s\r\nConnection: close\r\n\r\n".into(),
        ])
        .await;
        let empty = format!("{:x}", Sha256::digest([]));
        assert_eq!(
            cos.verified_sha256("group", "empty", 0, "s", &empty)
                .await
                .unwrap(),
            empty
        );
    }
}

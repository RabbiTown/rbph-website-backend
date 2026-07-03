use std::{collections::HashSet, time::Duration};

use futures_util::{FutureExt, future::BoxFuture};
use serde::{Deserialize, Serialize};

use super::{CaptchaAction, CaptchaProvider, CaptchaPublicConfig, CaptchaVerifyError};

const VERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const TEST_SITE_KEYS: [&str; 5] = [
    "1x00000000000000000000AA",
    "2x00000000000000000000AB",
    "1x00000000000000000000BB",
    "2x00000000000000000000BB",
    "3x00000000000000000000FF",
];
const TEST_SECRET_KEYS: [&str; 3] = [
    "1x0000000000000000000000000000000AA",
    "2x0000000000000000000000000000000AA",
    "3x0000000000000000000000000000000AA",
];

pub struct CloudflareCaptchaProvider {
    client: reqwest::Client,
    site_key: String,
    secret_key: String,
    allowed_hostnames: HashSet<String>,
    test_mode: bool,
}

#[derive(Serialize)]
struct VerifyRequest<'a> {
    secret: &'a str,
    response: &'a str,
    idempotency_key: String,
}

#[derive(Deserialize)]
struct VerifyResponse {
    success: bool,
    hostname: Option<String>,
    action: Option<String>,
    #[serde(rename = "error-codes", default)]
    error_codes: Vec<String>,
}

impl CloudflareCaptchaProvider {
    pub fn new(
        site_key: &str,
        secret_key: &str,
        allowed_hostnames: &[String],
        test_mode: bool,
    ) -> Result<Self, String> {
        let site_key = site_key.trim();
        let secret_key = secret_key.trim();
        let allowed_hostnames = allowed_hostnames
            .iter()
            .map(|hostname| hostname.trim().to_lowercase())
            .filter(|hostname| !hostname.is_empty())
            .collect::<HashSet<_>>();
        if site_key.is_empty() || secret_key.is_empty() || allowed_hostnames.is_empty() {
            return Err(
                "Cloudflare captcha requires site_key, secret_key and allowed_hostnames"
                    .to_string(),
            );
        }
        if test_mode
            && (!TEST_SITE_KEYS.contains(&site_key) || !TEST_SECRET_KEYS.contains(&secret_key))
        {
            return Err("Cloudflare captcha test_mode requires official test keys".to_string());
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|error| format!("Failed to create captcha HTTP client: {error}"))?;
        Ok(Self {
            client,
            site_key: site_key.to_string(),
            secret_key: secret_key.to_string(),
            allowed_hostnames,
            test_mode,
        })
    }

    fn response_is_valid(&self, response: &VerifyResponse, action: CaptchaAction) -> bool {
        response.success
            && (self.test_mode
                || response.action.as_deref() == Some(action.as_str())
                    && response.hostname.as_deref().is_some_and(|hostname| {
                        self.allowed_hostnames.contains(&hostname.to_lowercase())
                    }))
    }
}

impl CaptchaProvider for CloudflareCaptchaProvider {
    fn public_config(&self) -> CaptchaPublicConfig {
        CaptchaPublicConfig {
            provider: "cloudflare",
            site_key: self.site_key.clone(),
        }
    }

    fn verify<'a>(
        &'a self,
        token: &'a str,
        action: CaptchaAction,
    ) -> BoxFuture<'a, Result<(), CaptchaVerifyError>> {
        async move {
            let response = self
                .client
                .post(VERIFY_URL)
                .json(&VerifyRequest {
                    secret: &self.secret_key,
                    response: token,
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                })
                .send()
                .await
                .map_err(|error| {
                    log::warn!("Captcha verification request failed: {error}");
                    CaptchaVerifyError::Unavailable
                })?;
            if !response.status().is_success() {
                log::warn!("Captcha verification returned HTTP {}", response.status());
                return Err(CaptchaVerifyError::Unavailable);
            }
            let response = response.json::<VerifyResponse>().await.map_err(|error| {
                log::warn!("Invalid captcha verification response: {error}");
                CaptchaVerifyError::Unavailable
            })?;
            if self.response_is_valid(&response, action) {
                Ok(())
            } else {
                log::warn!(
                    "Captcha verification rejected: success={}, hostname={:?}, action={:?}, errors={:?}",
                    response.success,
                    response.hostname,
                    response.action,
                    response.error_codes,
                );
                Err(CaptchaVerifyError::Invalid)
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::{CloudflareCaptchaProvider, VerifyResponse};
    use crate::module::captcha::CaptchaAction;

    fn provider() -> CloudflareCaptchaProvider {
        CloudflareCaptchaProvider::new(
            "site-key",
            "secret-key",
            &["Example.COM".to_string()],
            false,
        )
        .unwrap()
    }

    #[test]
    fn validates_action_and_hostname() {
        let provider = provider();
        assert!(provider.response_is_valid(
            &VerifyResponse {
                success: true,
                hostname: Some("example.com".to_string()),
                action: Some("login".to_string()),
                error_codes: Vec::new(),
            },
            CaptchaAction::Login,
        ));
        assert!(!provider.response_is_valid(
            &VerifyResponse {
                success: true,
                hostname: Some("other.example".to_string()),
                action: Some("login".to_string()),
                error_codes: Vec::new(),
            },
            CaptchaAction::Login,
        ));
        assert!(!provider.response_is_valid(
            &VerifyResponse {
                success: true,
                hostname: Some("example.com".to_string()),
                action: Some("register".to_string()),
                error_codes: Vec::new(),
            },
            CaptchaAction::Login,
        ));
    }

    #[test]
    fn accepts_synthetic_metadata_only_with_official_test_keys() {
        let response = VerifyResponse {
            success: true,
            hostname: Some("example.com".to_string()),
            action: None,
            error_codes: Vec::new(),
        };
        let production = CloudflareCaptchaProvider::new(
            "site-key",
            "secret-key",
            &["localhost".to_string()],
            false,
        )
        .unwrap();
        let testing = CloudflareCaptchaProvider::new(
            "1x00000000000000000000AA",
            "1x0000000000000000000000000000000AA",
            &["localhost".to_string()],
            true,
        )
        .unwrap();

        assert!(!production.response_is_valid(&response, CaptchaAction::Login));
        assert!(testing.response_is_valid(&response, CaptchaAction::Login));
    }

    #[test]
    fn rejects_test_mode_with_non_test_keys() {
        assert!(
            CloudflareCaptchaProvider::new(
                "production-site-key",
                "production-secret-key",
                &["example.com".to_string()],
                true,
            )
            .is_err()
        );
    }
}

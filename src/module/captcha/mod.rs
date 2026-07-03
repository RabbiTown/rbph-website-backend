mod cloudflare;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Serialize;

use crate::config::CaptchaConfig;

use self::cloudflare::CloudflareCaptchaProvider;

#[derive(Clone, Copy)]
pub enum CaptchaAction {
    Login,
    Register,
}

impl CaptchaAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Register => "register",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptchaVerifyError {
    Invalid,
    Unavailable,
}

#[derive(Clone, Serialize)]
pub struct CaptchaPublicConfig {
    pub provider: &'static str,
    pub site_key: String,
}

trait CaptchaProvider: Send + Sync {
    fn public_config(&self) -> CaptchaPublicConfig;

    fn verify<'a>(
        &'a self,
        token: &'a str,
        action: CaptchaAction,
    ) -> BoxFuture<'a, Result<(), CaptchaVerifyError>>;
}

#[derive(Clone)]
pub struct CaptchaService {
    provider: Arc<dyn CaptchaProvider>,
}

impl CaptchaService {
    pub fn from_config(config: &CaptchaConfig) -> Result<Option<Self>, String> {
        let provider: Arc<dyn CaptchaProvider> = match config {
            CaptchaConfig::Disabled => return Ok(None),
            CaptchaConfig::Cloudflare {
                site_key,
                secret_key,
                allowed_hostnames,
                test_mode,
            } => Arc::new(CloudflareCaptchaProvider::new(
                site_key,
                secret_key,
                allowed_hostnames,
                *test_mode,
            )?),
        };
        Ok(Some(Self { provider }))
    }

    pub fn public_config(&self) -> CaptchaPublicConfig {
        self.provider.public_config()
    }

    pub async fn verify(
        &self,
        token: Option<&str>,
        action: CaptchaAction,
    ) -> Result<(), CaptchaVerifyError> {
        let token = token
            .map(str::trim)
            .filter(|token| !token.is_empty() && token.len() <= 2048)
            .ok_or(CaptchaVerifyError::Invalid)?;
        self.provider.verify(token, action).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_util::future::BoxFuture;

    use super::{
        CaptchaAction, CaptchaProvider, CaptchaPublicConfig, CaptchaService, CaptchaVerifyError,
    };

    struct AcceptProvider;

    impl CaptchaProvider for AcceptProvider {
        fn public_config(&self) -> CaptchaPublicConfig {
            CaptchaPublicConfig {
                provider: "test",
                site_key: "test".to_string(),
            }
        }

        fn verify<'a>(
            &'a self,
            _token: &'a str,
            _action: CaptchaAction,
        ) -> BoxFuture<'a, Result<(), CaptchaVerifyError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn service() -> CaptchaService {
        CaptchaService {
            provider: Arc::new(AcceptProvider),
        }
    }

    #[tokio::test]
    async fn rejects_missing_and_oversized_tokens_before_provider() {
        let service = service();
        assert_eq!(
            service.verify(None, CaptchaAction::Login).await,
            Err(CaptchaVerifyError::Invalid),
        );
        assert_eq!(
            service
                .verify(Some(&"x".repeat(2049)), CaptchaAction::Login)
                .await,
            Err(CaptchaVerifyError::Invalid),
        );
        assert_eq!(
            service.verify(Some("token"), CaptchaAction::Register).await,
            Ok(()),
        );
    }
}

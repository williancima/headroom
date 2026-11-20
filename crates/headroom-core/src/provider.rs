//! Errors shared by every provider client.

use crate::oauth::OAuthError;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ProviderError {
    /// The OAuth sign-in was refused or has expired.
    #[error("Sign-in expired. Sign in again.")]
    SignInExpired,
    /// An API key or token was refused; the message names the provider.
    #[error("{0}")]
    KeyRejected(String),
    #[error("Connection failed. Check your internet connection and try again.")]
    ConnectionFailed,
    #[error("The provider returned an invalid response.")]
    InvalidResponse,
    #[error("The provider request failed (HTTP {0}).")]
    RequestFailed(u16),
    #[error("The provider asked Headroom to wait before asking again.")]
    RateLimited { retry_after_secs: Option<u64> },
    /// A refusal that signing in again will not fix (e.g. missing token scope).
    #[error("{0}")]
    Unavailable(String),
    /// A hub request that got no answer; the message names the hub.
    #[error("{0}")]
    Unreachable(String),
}

impl From<OAuthError> for ProviderError {
    fn from(error: OAuthError) -> Self {
        match error {
            OAuthError::SignInExpired => Self::SignInExpired,
            OAuthError::Network => Self::ConnectionFailed,
            OAuthError::InvalidResponse => Self::InvalidResponse,
            OAuthError::RequestFailed(status) => Self::RequestFailed(status),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// A response from a provider that authenticates with an API key (Ollama, Z.ai).
pub struct KeyResponse {
    pub status: u16,
    pub body: Vec<u8>,
    retry_after: Option<String>,
    rate_limit_exhausted: bool,
}

impl KeyResponse {
    pub async fn read(response: reqwest::Response) -> Result<Self, ProviderError> {
        let status = response.status().as_u16();
        let headers = response.headers();
        let retry_after = headers.get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(String::from);
        let rate_limit_exhausted = headers.get("x-ratelimit-remaining").is_some_and(|v| v == "0");
        let body = response.bytes().await.map_err(|_| ProviderError::ConnectionFailed)?.to_vec();
        Ok(Self { status, body, retry_after, rate_limit_exhausted })
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403 && (self.rate_limit_exhausted || self.retry_after.is_some()))
    }

    /// 401, or 403 that is not a rate limit: `KeyRejected("<provider> rejected the API key.")`. A rate limit
    /// is `RateLimited`. Other non-2xx: `RequestFailed`.
    pub fn require_success(&self, provider: &str) -> Result<&Self, ProviderError> {
        if self.rate_limited() {
            return Err(ProviderError::RateLimited { retry_after_secs: self.retry_after.as_ref().and_then(|v| v.trim().parse().ok()) });
        }
        match self.status {
            401 | 403 => Err(ProviderError::KeyRejected(format!("{provider} rejected the API key."))),
            200..=299 => Ok(self),
            s => Err(ProviderError::RequestFailed(s)),
        }
    }
}

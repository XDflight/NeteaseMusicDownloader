use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("malformed response: {0}")]
    Json(#[from] serde_json::Error),

    #[error("crypto error: {0}")]
    Crypto(String),

    /// The server answered, but with a non-success `code`.
    #[error("{}", .0)]
    Api(ApiError),

    #[error("login required")]
    NeedLogin,

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: i64,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "server returned code {}", self.code)
        } else {
            write!(f, "{} (code {})", self.message, self.code)
        }
    }
}

impl Error {
    pub fn api(code: i64, message: impl Into<String>) -> Self {
        Error::Api(ApiError { code, message: message.into() })
    }

    /// Codes the service uses when it throttles or flags a client. Callers should back off.
    pub fn is_throttled(&self) -> bool {
        matches!(self, Error::Api(ApiError { code, .. }) if matches!(*code, -460 | -462 | 405 | 406 | 429 | 503 | 8821))
    }

    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(e) => e.is_timeout() || e.is_connect() || e.is_request() || e.is_body(),
            Error::Api(ApiError { code, .. }) => matches!(*code, 500..=504),
            _ => false,
        }
    }
}

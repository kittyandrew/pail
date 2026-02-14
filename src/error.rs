use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    ReadFile(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("validation error: {0}")]
    Validation(String),
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("HTTP request failed for {url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("failed to parse feed from {url}: {message}")]
    Parse { url: String, message: String },
}

#[derive(Debug, Error)]
pub enum GenerationError {
    #[error("opencode binary not found: {0}")]
    OpencodeBinaryNotFound(String),
    #[error("opencode invocation failed (exit code {exit_code:?}): {stderr}")]
    OpencodeExecution { exit_code: Option<i32>, stderr: String },
    #[error("opencode timed out after {0}")]
    Timeout(String),
    #[error("failed to parse output: {0}")]
    OutputParse(String),
    #[error("workspace preparation failed: {0}")]
    Workspace(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("failed to connect to Telegram: {0}")]
    Connection(String),
}

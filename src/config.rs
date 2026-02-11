use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::error::ConfigError;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub pail: PailConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub opencode: OpencodeConfig,
    #[serde(default)]
    pub source: Vec<SourceConfig>,
    #[serde(default)]
    pub output_channel: Vec<OutputChannelConfig>,
}

#[derive(Debug, Deserialize)]
pub struct PailConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_retention")]
    pub retention: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_generations: u32,
}

fn default_version() -> u32 {
    1
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}
fn default_retention() -> String {
    "7d".to_string()
}
fn default_timezone() -> String {
    "UTC".to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_max_concurrent() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
        }
    }
}

fn default_db_path() -> String {
    "pail.db".to_string()
}

#[derive(Debug, Deserialize)]
pub struct OpencodeConfig {
    #[serde(default = "default_opencode_binary")]
    pub binary: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for OpencodeConfig {
    fn default() -> Self {
        Self {
            binary: default_opencode_binary(),
            default_model: None,
            timeout: default_timeout(),
            max_retries: default_max_retries(),
            extra_args: Vec::new(),
        }
    }
}

fn default_opencode_binary() -> String {
    "opencode".to_string()
}
fn default_timeout() -> String {
    "10m".to_string()
}
fn default_max_retries() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: String,
    #[serde(default = "default_max_items")]
    pub max_items: u32,
    pub auth: Option<SourceAuthConfig>,
    #[serde(default = "default_enabled")]
    pub enabled: Option<bool>,
}

fn default_poll_interval() -> String {
    "30m".to_string()
}
fn default_max_items() -> u32 {
    200
}
fn default_enabled() -> Option<bool> {
    Some(true)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceAuthConfig {
    #[serde(rename = "type")]
    pub auth_type: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub header_name: Option<String>,
    pub header_value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OutputChannelConfig {
    pub name: String,
    pub slug: String,
    pub schedule: String,
    pub sources: Vec<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub language: Option<String>,
    #[serde(default = "default_channel_enabled")]
    pub enabled: Option<bool>,
}

fn default_channel_enabled() -> Option<bool> {
    Some(true)
}

impl Config {
    /// Resolve the database path (relative to data_dir if not absolute).
    pub fn db_path(&self) -> PathBuf {
        let db_path = Path::new(&self.database.path);
        if db_path.is_absolute() {
            db_path.to_path_buf()
        } else {
            self.pail.data_dir.join(db_path)
        }
    }
}

pub fn load_config(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .map_err(ConfigError::ReadFile)
        .context("reading config file")?;
    let config: Config = toml::from_str(&content).map_err(ConfigError::Parse)?;
    Ok(config)
}

pub fn validate_config(config: &Config) -> Result<()> {
    // Validate config version
    if config.pail.version != 1 {
        return Err(ConfigError::Validation(format!(
            "unsupported config version {} (this binary supports version 1)",
            config.pail.version
        ))
        .into());
    }

    // Validate source types
    for source in &config.source {
        match source.source_type.as_str() {
            "rss" => {
                if source.url.is_none() {
                    return Err(ConfigError::Validation(format!(
                        "source '{}': RSS source must have a 'url'",
                        source.name
                    ))
                    .into());
                }
            }
            "telegram_channel" | "telegram_group" | "telegram_folder" => {
                // TG sources not supported in Phase 1a, but don't reject them
            }
            other => {
                return Err(
                    ConfigError::Validation(format!("source '{}': unknown type '{}'", source.name, other)).into(),
                );
            }
        }

        // Validate auth config
        if let Some(auth) = &source.auth {
            match auth.auth_type.as_str() {
                "basic" => {
                    if auth.username.is_none() || auth.password.is_none() {
                        return Err(ConfigError::Validation(format!(
                            "source '{}': basic auth requires 'username' and 'password'",
                            source.name
                        ))
                        .into());
                    }
                }
                "bearer" => {
                    if auth.token.is_none() {
                        return Err(ConfigError::Validation(format!(
                            "source '{}': bearer auth requires 'token'",
                            source.name
                        ))
                        .into());
                    }
                }
                "header" => {
                    if auth.header_name.is_none() || auth.header_value.is_none() {
                        return Err(ConfigError::Validation(format!(
                            "source '{}': header auth requires 'header_name' and 'header_value'",
                            source.name
                        ))
                        .into());
                    }
                }
                other => {
                    return Err(ConfigError::Validation(format!(
                        "source '{}': unknown auth type '{}'",
                        source.name, other
                    ))
                    .into());
                }
            }
        }

        // Validate max_items fits in i32 (SQLite INTEGER)
        if source.max_items > i32::MAX as u32 {
            return Err(ConfigError::Validation(format!(
                "source '{}': max_items {} exceeds maximum ({})",
                source.name,
                source.max_items,
                i32::MAX
            ))
            .into());
        }

        // Validate poll_interval is parseable
        humantime::parse_duration(&source.poll_interval).map_err(|e| {
            ConfigError::Validation(format!(
                "source '{}': invalid poll_interval '{}': {}",
                source.name, source.poll_interval, e
            ))
        })?;
    }

    // Validate source names are unique
    let mut source_names = HashSet::new();
    for source in &config.source {
        if !source_names.insert(&source.name) {
            return Err(ConfigError::Validation(format!("duplicate source name: '{}'", source.name)).into());
        }
    }

    // Validate output channels
    let mut channel_slugs = HashSet::new();
    for channel in &config.output_channel {
        if !channel_slugs.insert(&channel.slug) {
            return Err(ConfigError::Validation(format!("duplicate output channel slug: '{}'", channel.slug)).into());
        }

        // Validate slug is URL-safe (used in feed paths: /feed/<username>/<slug>.atom)
        if !channel
            .slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || channel.slug.is_empty()
            || channel.slug.starts_with('-')
            || channel.slug.ends_with('-')
        {
            return Err(ConfigError::Validation(format!(
                "output channel '{}': slug '{}' must be non-empty, contain only lowercase letters, digits, and hyphens, \
                 and not start or end with a hyphen",
                channel.name, channel.slug
            ))
            .into());
        }

        if channel.sources.is_empty() {
            return Err(ConfigError::Validation(format!(
                "output channel '{}': must have at least one source",
                channel.name
            ))
            .into());
        }

        // Validate source references exist
        for source_name in &channel.sources {
            if !source_names.contains(source_name) {
                return Err(ConfigError::Validation(format!(
                    "output channel '{}': references unknown source '{}'",
                    channel.name, source_name
                ))
                .into());
            }
        }

        // Validate schedule expression
        validate_schedule(&channel.schedule)
            .map_err(|e| ConfigError::Validation(format!("output channel '{}': {}", channel.name, e)))?;
    }

    // Validate timezone
    config
        .pail
        .timezone
        .parse::<chrono_tz::Tz>()
        .map_err(|_| ConfigError::Validation(format!("unknown timezone '{}'", config.pail.timezone)))?;

    // Validate opencode timeout
    humantime::parse_duration(&config.opencode.timeout)
        .map_err(|e| ConfigError::Validation(format!("opencode timeout '{}': {}", config.opencode.timeout, e)))?;

    // Validate retention
    humantime::parse_duration(&config.pail.retention)
        .map_err(|e| ConfigError::Validation(format!("retention '{}': {}", config.pail.retention, e)))?;

    Ok(())
}

/// Validate a schedule expression.
/// Supported formats: "at:HH:MM[,HH:MM...]", "weekly:DAY,HH:MM", "cron:EXPR"
fn validate_schedule(schedule: &str) -> Result<(), String> {
    if let Some(times) = schedule.strip_prefix("at:") {
        for time_str in times.split(',') {
            validate_time(time_str.trim())?;
        }
        Ok(())
    } else if let Some(rest) = schedule.strip_prefix("weekly:") {
        let parts: Vec<&str> = rest.splitn(2, ',').collect();
        if parts.len() != 2 {
            return Err(format!(
                "invalid weekly schedule '{schedule}': expected 'weekly:DAY,HH:MM'"
            ));
        }
        let day = parts[0].trim().to_lowercase();
        let valid_days = [
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ];
        if !valid_days.contains(&day.as_str()) {
            return Err(format!("invalid day '{day}' in schedule '{schedule}'"));
        }
        validate_time(parts[1].trim())?;
        Ok(())
    } else if schedule.starts_with("cron:") {
        // Accept cron expressions without deep validation
        Ok(())
    } else {
        Err(format!(
            "invalid schedule '{schedule}': must start with 'at:', 'weekly:', or 'cron:'"
        ))
    }
}

fn validate_time(time_str: &str) -> Result<(), String> {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 {
        return Err(format!("invalid time '{time_str}': expected HH:MM"));
    }
    let hour: u32 = parts[0].parse().map_err(|_| format!("invalid hour in '{time_str}'"))?;
    let minute: u32 = parts[1]
        .parse()
        .map_err(|_| format!("invalid minute in '{time_str}'"))?;
    if hour > 23 {
        return Err(format!("hour {hour} out of range in '{time_str}'"));
    }
    if minute > 59 {
        return Err(format!("minute {minute} out of range in '{time_str}'"));
    }
    Ok(())
}

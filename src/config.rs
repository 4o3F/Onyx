use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Deserializer};

/// Application configuration loaded from a TOML file.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Logging-related configuration.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Base URL of the target system under test.
    pub base_url: String,
    /// Path to the team credentials CSV file (`id,username,password`).
    pub team_csv: String,
    /// Contest machine-readable identifier (`id`) used for pre-flight validation.
    pub contest_id: String,
    /// Root directory containing per-problem submission source files.
    #[serde(default = "default_solutions_path")]
    pub solutions_path: String,
    /// Probability of submitting a wrong solution (`TLE`) each submission, in range [0.0, 1.0].
    #[serde(default = "default_wrong_solution_probability")]
    pub wrong_solution_probability: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            logging: LoggingConfig::default(),
            base_url: String::new(),
            team_csv: String::new(),
            contest_id: String::new(),
            solutions_path: default_solutions_path(),
            wrong_solution_probability: default_wrong_solution_probability(),
        }
    }
}

/// Logging configuration section.
#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    /// Maximum log level emitted by tracing.
    #[serde(
        default = "default_log_level",
        deserialize_with = "deserialize_log_level"
    )]
    pub level: tracing::Level,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

/// Default tracing log level.
fn default_log_level() -> tracing::Level {
    tracing::Level::TRACE
}

/// Default root directory for submission source files.
fn default_solutions_path() -> String {
    "./solutions".to_string()
}

/// Default probability of selecting wrong (`TLE`) solution on a submission.
fn default_wrong_solution_probability() -> f64 {
    0.0
}

/// Load and parse application configuration from a TOML file.
pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str::<Config>(&content)
        .with_context(|| format!("failed to parse TOML config file: {}", path.display()))
}

/// Deserialize `logging.level` from string into `tracing::Level`.
fn deserialize_log_level<'de, D>(deserializer: D) -> Result<tracing::Level, D::Error>
where
    D: Deserializer<'de>,
{
    let level = String::deserialize(deserializer)?;
    parse_log_level(&level).map_err(serde::de::Error::custom)
}

/// Parse a case-insensitive log-level string.
fn parse_log_level(level: &str) -> Result<tracing::Level, String> {
    match level.to_ascii_lowercase().as_str() {
        "trace" => Ok(tracing::Level::TRACE),
        "debug" => Ok(tracing::Level::DEBUG),
        "info" => Ok(tracing::Level::INFO),
        "warn" => Ok(tracing::Level::WARN),
        "error" => Ok(tracing::Level::ERROR),
        _ => Err(format!("unsupported logging.level '{}'", level)),
    }
}

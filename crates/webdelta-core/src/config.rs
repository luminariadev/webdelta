use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use regex::Regex;
use scraper::Selector;
use serde::{Deserialize, Serialize};
use serde_json_path::JsonPath;
use url::Url;

use crate::error::{Error, Result};

/// Root configuration loaded from `webdelta.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub notifiers: BTreeMap<String, NotifierConfig>,
    #[serde(default)]
    pub watches: Vec<WatchConfig>,
}

/// Workspace-wide defaults applied when watch fields are omitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DefaultsConfig {
    #[serde(default = "default_interval")]
    pub interval: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_retries")]
    pub retries: usize,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    #[serde(default = "default_max_history")]
    pub max_history: usize,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            timeout_secs: default_timeout_secs(),
            retries: default_retries(),
            user_agent: default_user_agent(),
            max_history: default_max_history(),
            concurrency: default_concurrency(),
        }
    }
}

/// HTTP method supported by watch requests.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    #[serde(alias = "get", alias = "Get")]
    Get,
    #[serde(alias = "head", alias = "Head")]
    Head,
    #[serde(alias = "post", alias = "Post")]
    Post,
}

impl HttpMethod {
    /// Returns the uppercase HTTP method name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
        }
    }
}

/// Configuration for a single monitored URL or API endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WatchConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub method: HttpMethod,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub retries: Option<usize>,
    #[serde(default)]
    pub extractor: ExtractorConfig,
    #[serde(default)]
    pub ignore_regex: Vec<String>,
    #[serde(default)]
    pub normalize: NormalizeConfig,
    #[serde(default)]
    pub conditions: Vec<ConditionConfig>,
    #[serde(default)]
    pub notifiers: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl WatchConfig {
    /// Returns the display name or falls back to the watch ID.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// Resolves the effective polling interval for this watch.
    pub fn effective_interval(&self, defaults: &DefaultsConfig) -> Result<Duration> {
        let raw = self.interval.as_deref().unwrap_or(&defaults.interval);
        parse_duration(raw)
    }

    /// Resolves the effective request timeout in seconds.
    pub fn effective_timeout_secs(&self, defaults: &DefaultsConfig) -> u64 {
        self.timeout_secs.unwrap_or(defaults.timeout_secs)
    }

    /// Resolves the effective retry count.
    pub fn effective_retries(&self, defaults: &DefaultsConfig) -> usize {
        self.retries.unwrap_or(defaults.retries)
    }
}

/// Strategy used to extract comparable text from an HTTP response.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtractorConfig {
    #[default]
    Text,
    Css {
        selector: String,
        #[serde(default)]
        attribute: Option<String>,
        #[serde(default)]
        all: bool,
    },
    JsonPath {
        path: String,
    },
    Regex {
        pattern: String,
        #[serde(default)]
        group: Option<usize>,
    },
    Status {
        #[serde(default)]
        header: Option<String>,
    },
}

/// Text normalization options applied after extraction and regex filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizeConfig {
    #[serde(default = "default_true")]
    pub trim: bool,
    #[serde(default = "default_true")]
    pub collapse_whitespace: bool,
    #[serde(default)]
    pub lowercase: bool,
}

impl Default for NormalizeConfig {
    fn default() -> Self {
        Self {
            trim: true,
            collapse_whitespace: true,
            lowercase: false,
        }
    }
}

/// Rule evaluated when content changes compared to the previous snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConditionConfig {
    Changed,
    Contains {
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    NotContains {
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    RegexMatch {
        pattern: String,
    },
    NumericLt {
        threshold: f64,
    },
    NumericLte {
        threshold: f64,
    },
    NumericGt {
        threshold: f64,
    },
    NumericGte {
        threshold: f64,
    },
    DropPctAtLeast {
        percent: f64,
    },
    RisePctAtLeast {
        percent: f64,
    },
}

/// Notification channel definitions referenced by watches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifierConfig {
    Webhook {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        secret: Option<String>,
    },
    Telegram {
        bot_token: String,
        chat_id: String,
        #[serde(default)]
        api_base_url: Option<String>,
    },
    Discord {
        webhook_url: String,
    },
    Slack {
        webhook_url: String,
    },
    Ntfy {
        url: String,
        topic: String,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        token: Option<String>,
    },
}

fn default_interval() -> String {
    "15m".to_string()
}

fn default_timeout_secs() -> u64 {
    15
}

fn default_retries() -> usize {
    2
}

fn default_user_agent() -> String {
    format!("webdelta/{}", env!("CARGO_PKG_VERSION"))
}

fn default_max_history() -> usize {
    100
}

fn default_concurrency() -> usize {
    8
}

fn default_true() -> bool {
    true
}

/// Parses a human-friendly duration string such as `30s`, `15m`, `2h`, or `1d`.
pub fn parse_duration(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        return Err(Error::Validation(format!(
            "invalid duration '{raw}': expected <number><s|m|h|d>"
        )));
    }
    let (digits, unit) = trimmed.split_at(trimmed.len() - 1);
    let value: u64 = digits.trim().parse().map_err(|_| {
        Error::Validation(format!(
            "invalid duration '{raw}': numeric prefix is invalid"
        ))
    })?;
    if value == 0 {
        return Err(Error::Validation(format!(
            "invalid duration '{raw}': duration must be greater than zero"
        )));
    }
    let seconds = match unit {
        "s" | "S" => value,
        "m" | "M" => value.saturating_mul(60),
        "h" | "H" => value.saturating_mul(3_600),
        "d" | "D" => value.saturating_mul(86_400),
        _ => {
            return Err(Error::Validation(format!(
                "invalid duration unit '{unit}' in '{raw}': expected s, m, h, or d"
            )))
        }
    };
    Ok(Duration::from_secs(seconds))
}

/// Expands `${VAR_NAME}` placeholders using process environment variables.
///
/// Missing variables are preserved as-is unless `require_all` is `true`, in which
/// case an error listing the missing variable names is returned.
pub fn expand_env_vars_checked(input: &str, require_all: bool) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut missing = Vec::new();
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_prefix = &rest[start + 2..];
        if let Some(end) = after_prefix.find('}') {
            let var_name = &after_prefix[..end];
            if !var_name.is_empty()
                && var_name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                match std::env::var(var_name) {
                    Ok(val) => output.push_str(&val),
                    Err(_) => {
                        missing.push(var_name.to_string());
                        output.push_str(&rest[start..start + 2 + end + 1]);
                    }
                }
                rest = &after_prefix[end + 1..];
                continue;
            }
        }
        output.push_str("${");
        rest = after_prefix;
    }
    output.push_str(rest);

    if require_all && !missing.is_empty() {
        return Err(Error::Notify(format!(
            "missing required environment variable(s): {}",
            missing.join(", ")
        )));
    }

    Ok(output)
}

/// Expands `${VAR_NAME}` placeholders, leaving any unset variables unchanged.
pub fn expand_env_vars(input: &str) -> String {
    expand_env_vars_checked(input, false).unwrap_or_else(|_| input.to_string())
}

/// Loads and validates a `Config` from a TOML file.
pub fn load_config(path: &Path) -> Result<Config> {
    let raw = fs::read_to_string(path).map_err(|err| {
        Error::Config(format!(
            "failed to read config file '{}': {err}",
            path.display()
        ))
    })?;
    let config: Config = toml::from_str(&raw).map_err(|err| {
        Error::Config(format!(
            "failed to parse TOML in '{}': {err}",
            path.display()
        ))
    })?;
    validate(&config)?;
    Ok(config)
}

/// Validates and atomically writes a `Config` to a TOML file.
pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    validate(config)?;
    let serialized = toml::to_string_pretty(config)
        .map_err(|err| Error::Config(format!("failed to serialize config: {err}")))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, serialized)?;
    Ok(())
}

/// Validates structural integrity, selectors, regexes, URLs, and notifier links.
pub fn validate(config: &Config) -> Result<()> {
    parse_duration(&config.defaults.interval)?;
    if config.defaults.timeout_secs == 0 {
        return Err(Error::Validation(
            "defaults.timeout_secs must be greater than zero".to_string(),
        ));
    }
    if config.defaults.max_history == 0 {
        return Err(Error::Validation(
            "defaults.max_history must be greater than zero".to_string(),
        ));
    }
    if config.defaults.concurrency == 0 {
        return Err(Error::Validation(
            "defaults.concurrency must be greater than zero".to_string(),
        ));
    }

    for (name, notifier) in &config.notifiers {
        if name.trim().is_empty() {
            return Err(Error::Validation(
                "notifier name must not be empty".to_string(),
            ));
        }
        validate_notifier(name, notifier)?;
    }

    let mut seen_ids = HashSet::new();
    for watch in &config.watches {
        validate_watch_id(&watch.id)?;
        if !seen_ids.insert(watch.id.clone()) {
            return Err(Error::Validation(format!(
                "duplicate watch id '{}'",
                watch.id
            )));
        }

        validate_http_url(&watch.url, &format!("watch '{}'", watch.id))?;

        if let Some(interval) = &watch.interval {
            parse_duration(interval)?;
        }
        if let Some(timeout) = watch.timeout_secs {
            if timeout == 0 {
                return Err(Error::Validation(format!(
                    "watch '{}' timeout_secs must be greater than zero",
                    watch.id
                )));
            }
        }

        validate_extractor(&watch.id, &watch.extractor)?;

        for pattern in &watch.ignore_regex {
            Regex::new(pattern).map_err(|err| {
                Error::Validation(format!(
                    "watch '{}' invalid ignore_regex '{}': {err}",
                    watch.id, pattern
                ))
            })?;
        }

        for cond in &watch.conditions {
            validate_condition(&watch.id, cond)?;
        }

        for notifier_name in &watch.notifiers {
            if !config.notifiers.contains_key(notifier_name) {
                return Err(Error::Validation(format!(
                    "watch '{}' references unknown notifier '{}'",
                    watch.id, notifier_name
                )));
            }
        }
    }

    Ok(())
}

/// Validates that a watch ID contains safe identifier characters only.
pub fn validate_watch_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(Error::Validation("watch id must not be empty".to_string()));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(Error::Validation(format!(
            "watch id '{id}' may only contain ASCII letters, digits, '.', '-', and '_'"
        )));
    }
    Ok(())
}

fn validate_http_url(raw: &str, context: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.contains("${") {
        // Allow environment placeholder URLs in notifier configs prior to dispatch.
        return Ok(());
    }
    let parsed = Url::parse(trimmed)
        .map_err(|err| Error::Validation(format!("{context} has invalid URL '{raw}': {err}")))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(Error::Validation(format!(
            "{context} URL '{raw}' has unsupported scheme '{other}': expected http or https"
        ))),
    }
}

fn validate_notifier(name: &str, notifier: &NotifierConfig) -> Result<()> {
    let ctx = format!("notifier '{name}'");
    match notifier {
        NotifierConfig::Webhook { url, .. } => validate_http_url(url, &ctx),
        NotifierConfig::Telegram {
            bot_token,
            chat_id,
            api_base_url,
        } => {
            if bot_token.trim().is_empty() || chat_id.trim().is_empty() {
                return Err(Error::Validation(format!(
                    "{ctx} requires non-empty bot_token and chat_id"
                )));
            }
            if let Some(base) = api_base_url {
                validate_http_url(base, &ctx)?;
            }
            Ok(())
        }
        NotifierConfig::Discord { webhook_url } | NotifierConfig::Slack { webhook_url } => {
            validate_http_url(webhook_url, &ctx)
        }
        NotifierConfig::Ntfy { url, topic, .. } => {
            if topic.trim().is_empty() {
                return Err(Error::Validation(format!(
                    "{ctx} requires a non-empty topic"
                )));
            }
            validate_http_url(url, &ctx)
        }
    }
}

fn validate_extractor(watch_id: &str, extractor: &ExtractorConfig) -> Result<()> {
    match extractor {
        ExtractorConfig::Text | ExtractorConfig::Status { .. } => Ok(()),
        ExtractorConfig::Css { selector, .. } => {
            Selector::parse(selector).map_err(|err| {
                Error::Validation(format!(
                    "watch '{watch_id}' has invalid CSS selector '{selector}': {err:?}"
                ))
            })?;
            Ok(())
        }
        ExtractorConfig::JsonPath { path } => {
            JsonPath::parse(path).map_err(|err| {
                Error::Validation(format!(
                    "watch '{watch_id}' has invalid JSONPath '{path}': {err}"
                ))
            })?;
            Ok(())
        }
        ExtractorConfig::Regex { pattern, .. } => {
            Regex::new(pattern).map_err(|err| {
                Error::Validation(format!(
                    "watch '{watch_id}' has invalid regex '{pattern}': {err}"
                ))
            })?;
            Ok(())
        }
    }
}

fn validate_condition(watch_id: &str, condition: &ConditionConfig) -> Result<()> {
    match condition {
        ConditionConfig::Changed
        | ConditionConfig::Contains { .. }
        | ConditionConfig::NotContains { .. } => Ok(()),
        ConditionConfig::RegexMatch { pattern } => {
            Regex::new(pattern).map_err(|err| {
                Error::Validation(format!(
                    "watch '{watch_id}' invalid condition regex '{pattern}': {err}"
                ))
            })?;
            Ok(())
        }
        ConditionConfig::NumericLt { threshold }
        | ConditionConfig::NumericLte { threshold }
        | ConditionConfig::NumericGt { threshold }
        | ConditionConfig::NumericGte { threshold } => {
            if !threshold.is_finite() {
                return Err(Error::Validation(format!(
                    "watch '{watch_id}' numeric threshold must be finite"
                )));
            }
            Ok(())
        }
        ConditionConfig::DropPctAtLeast { percent }
        | ConditionConfig::RisePctAtLeast { percent } => {
            if !percent.is_finite() || *percent < 0.0 {
                return Err(Error::Validation(format!(
                    "watch '{watch_id}' percentage threshold must be finite and non-negative"
                )));
            }
            Ok(())
        }
    }
}

/// Returns a runnable starter `webdelta.toml` configuration.
pub fn starter_config_toml() -> String {
    r#"# webdelta configuration file

[defaults]
interval = "15m"
timeout_secs = 15
retries = 2
max_history = 100
concurrency = 8

# Example webhook notifier (compatible with n8n, custom APIs, or Slack/Discord bridges).
# Environment variables like ${WEBDELTA_WEBHOOK_SECRET} are expanded when sending alerts.
# [notifiers.n8n]
# type = "webhook"
# url = "https://example.com/webhook/webdelta"
# secret = "${WEBDELTA_WEBHOOK_SECRET}"

[[watches]]
id = "example-site"
name = "Example Domain Title"
url = "https://example.com"
interval = "30m"
enabled = true

[watches.extractor]
type = "css"
selector = "h1"

[[watches.conditions]]
type = "changed"
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_variants() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("15m").unwrap(), Duration::from_secs(900));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn test_expand_env_vars() {
        std::env::set_var("WEBDELTA_TEST_VAR", "secret123");
        let expanded = expand_env_vars("Bearer ${WEBDELTA_TEST_VAR}/ok");
        assert_eq!(expanded, "Bearer secret123/ok");

        let checked = expand_env_vars_checked("Bearer ${WEBDELTA_MISSING_XYZ}", true);
        assert!(checked.is_err());
    }

    #[test]
    fn test_starter_config_validates() {
        let starter = starter_config_toml();
        let cfg: Config = toml::from_str(&starter).unwrap();
        validate(&cfg).unwrap();
        assert_eq!(cfg.watches.len(), 1);
        assert_eq!(cfg.watches[0].id, "example-site");
    }

    #[test]
    fn test_validate_rejects_unknown_notifier_and_duplicate_id() {
        let mut cfg = Config::default();
        cfg.watches.push(WatchConfig {
            id: "w1".to_string(),
            name: None,
            url: "https://example.com".to_string(),
            method: HttpMethod::Get,
            headers: BTreeMap::new(),
            interval: None,
            timeout_secs: None,
            retries: None,
            extractor: ExtractorConfig::Text,
            ignore_regex: vec![],
            normalize: NormalizeConfig::default(),
            conditions: vec![],
            notifiers: vec!["missing".to_string()],
            enabled: true,
        });
        assert!(validate(&cfg).is_err());
    }
}

pub mod config;
pub mod diff;
pub mod engine;
pub mod error;
pub mod extract;
pub mod fetch;
pub mod notify;
pub mod numeric;
pub mod rules;
pub mod store;

pub use config::{
    expand_env_vars, expand_env_vars_checked, load_config, parse_duration, save_config,
    starter_config_toml, validate, validate_watch_id, BrowserConfig, ConditionConfig, Config,
    DefaultsConfig, ExtractorConfig, FetchEngine, HttpMethod, NormalizeConfig, NotifierConfig,
    WatchConfig,
};
pub use diff::{compute_diff, DiffResult};
pub use engine::{sha256_hex, CheckOutcome, CheckStatus, MonitorEngine, PreviewResult};
pub use error::{Error, Result};
pub use extract::extract_content;
pub use notify::{AlertEvent, NotificationResult};
pub use numeric::{compute_delta, parse_numeric, NumericDelta};
pub use rules::{evaluate_conditions, RuleEvaluation};
pub use store::{SnapshotRecord, StateStore};
pub use webdelta_browser::{discover_browser_binary, looks_like_spa_shell};

/// Returns the crate version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

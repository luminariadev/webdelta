use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use serde_json::json;
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;
use webdelta_core::{
    compute_delta, compute_diff, load_config, save_config, starter_config_toml, BrowserConfig,
    CheckOutcome, CheckStatus, ConditionConfig, Config, Error, ExtractorConfig, FetchEngine,
    HttpMethod, MonitorEngine, NormalizeConfig, Result, StateStore, WatchConfig,
};

#[derive(Debug, Parser)]
#[command(
    name = "webdelta",
    version,
    about = "Fast change detection and smart diff monitoring for websites and APIs"
)]
struct Cli {
    #[arg(
        short = 'c',
        long = "config",
        global = true,
        default_value = "webdelta.toml",
        help = "Path to configuration file"
    )]
    config: PathBuf,

    #[arg(
        long = "db",
        global = true,
        default_value = ".webdelta/state.redb",
        help = "Path to embedded state database"
    )]
    db: PathBuf,

    #[arg(long = "json", global = true, help = "Emit structured JSON output")]
    json: bool,

    #[arg(
        short = 'v',
        long = "verbose",
        global = true,
        help = "Enable verbose diagnostic logs on stderr"
    )]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a starter webdelta.toml configuration file
    Init {
        #[arg(long, help = "Overwrite an existing configuration file")]
        force: bool,
    },

    /// Validate syntax, selectors, regexes, and notifier references in webdelta.toml
    Validate,

    /// Add a new watch entry to the configuration file
    Add(AddArgs),

    /// List configured watches and their latest recorded snapshot
    List,

    /// Remove a watch entry from the configuration file
    Remove {
        id: String,
        #[arg(long, help = "Also delete stored snapshot history for this watch")]
        purge_history: bool,
    },

    /// Fetch a URL once and preview extracted output without saving state
    Preview(PreviewArgs),

    /// Run one-time checks against all enabled watches or specific watch IDs
    Check(CheckArgs),

    /// Show the unified diff between the two most recent snapshots of a watch
    Diff {
        id: String,
        #[arg(long, default_value_t = 3, help = "Unified diff context lines")]
        context: usize,
    },

    /// Show stored snapshot history for a watch
    History {
        id: String,
        #[arg(long, default_value_t = 20, help = "Maximum snapshots to display")]
        limit: usize,
    },

    /// Run the continuous polling scheduler until interrupted
    Watch {
        #[arg(long, help = "Override maximum concurrent HTTP requests")]
        concurrency: Option<usize>,
    },

    /// Start a Model Context Protocol stdio server
    Mcp,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("extractor_mode")
        .args(["css", "jsonpath", "regex", "status"])
        .multiple(false)
))]
struct ExtractorCliArgs {
    #[arg(long, help = "CSS selector to extract")]
    css: Option<String>,

    #[arg(long = "jsonpath", help = "JSONPath expression to extract")]
    jsonpath: Option<String>,

    #[arg(long, help = "Regular expression pattern to extract")]
    regex: Option<String>,

    #[arg(long, help = "Track HTTP status code (and optional header)")]
    status: bool,

    #[arg(long, help = "HTML attribute to read when using --css")]
    attr: Option<String>,

    #[arg(long, help = "Join all matches when using --css")]
    all: bool,

    #[arg(long, help = "Capture group index when using --regex")]
    group: Option<usize>,

    #[arg(long, help = "Optional response header name when using --status")]
    header: Option<String>,

    #[arg(
        long = "ignore-regex",
        help = "Regex pattern removed from extracted text before diffing"
    )]
    ignore_regex: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CliEngine {
    #[default]
    Http,
    Browser,
    Auto,
}

impl From<CliEngine> for FetchEngine {
    fn from(value: CliEngine) -> Self {
        match value {
            CliEngine::Http => Self::Http,
            CliEngine::Browser => Self::Browser,
            CliEngine::Auto => Self::Auto,
        }
    }
}

#[derive(Debug, Args)]
struct BrowserCliArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = CliEngine::Http,
        help = "Fetch engine: http, browser (headless CDP), or auto"
    )]
    engine: CliEngine,

    #[arg(
        long = "wait-selector",
        help = "Wait for CSS selector before reading DOM in browser mode"
    )]
    wait_selector: Option<String>,

    #[arg(
        long = "wait-ms",
        help = "Extra delay in milliseconds after page load in browser mode"
    )]
    wait_ms: Option<u64>,

    #[arg(
        long = "cdp-url",
        help = "Optional remote CDP HTTP or WebSocket endpoint"
    )]
    cdp_url: Option<String>,
}

impl BrowserCliArgs {
    fn to_browser_config(&self) -> BrowserConfig {
        BrowserConfig {
            wait_for_selector: self.wait_selector.clone(),
            wait_ms: self.wait_ms,
            cdp_url: self.cdp_url.clone(),
        }
    }
}

#[derive(Debug, Args)]
struct AddArgs {
    id: String,
    url: String,

    #[arg(long, help = "Human-readable label for this watch")]
    name: Option<String>,

    #[arg(long, help = "Polling interval such as 30s, 15m, 2h, or 1d")]
    interval: Option<String>,

    #[command(flatten)]
    extractor: ExtractorCliArgs,

    #[command(flatten)]
    browser: BrowserCliArgs,

    #[arg(
        long = "contains",
        help = "Require extracted text to contain substring"
    )]
    contains: Vec<String>,

    #[arg(
        long = "not-contains",
        help = "Require extracted text not to contain substring"
    )]
    not_contains: Vec<String>,

    #[arg(
        long = "lt",
        help = "Trigger only when parsed number is below threshold"
    )]
    lt: Option<f64>,

    #[arg(
        long = "gt",
        help = "Trigger only when parsed number is above threshold"
    )]
    gt: Option<f64>,

    #[arg(
        long = "drop-pct",
        help = "Trigger only when numeric value drops by at least N percent"
    )]
    drop_pct: Option<f64>,

    #[arg(
        long = "rise-pct",
        help = "Trigger only when numeric value rises by at least N percent"
    )]
    rise_pct: Option<f64>,

    #[arg(long = "notifier", help = "Notifier name defined under [notifiers]")]
    notifiers: Vec<String>,
}

#[derive(Debug, Args)]
struct PreviewArgs {
    url: String,

    #[command(flatten)]
    extractor: ExtractorCliArgs,

    #[command(flatten)]
    browser: BrowserCliArgs,

    #[arg(
        short = 'H',
        long = "request-header",
        help = "Extra HTTP request header formatted as 'Key: Value'"
    )]
    request_headers: Vec<String>,

    #[arg(long, default_value_t = 15, help = "Request timeout in seconds")]
    timeout: u64,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(help = "Optional watch IDs to check (defaults to all enabled watches)")]
    ids: Vec<String>,

    #[arg(long, help = "Override maximum concurrent HTTP requests")]
    concurrency: Option<usize>,

    #[arg(long = "no-notify", help = "Check without dispatching notifications")]
    no_notify: bool,

    #[arg(
        long = "exit-code",
        help = "Return exit code 2 when one or more watches report triggered changes"
    )]
    exit_code: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        "warn,webdelta_core=debug,webdelta_browser=debug,webdelta_mcp=debug,webdelta=debug"
    } else {
        "warn"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match &cli.command {
        Commands::Init { force } => cmd_init(&cli, *force),
        Commands::Validate => cmd_validate(&cli),
        Commands::Add(args) => cmd_add(&cli, args),
        Commands::List => cmd_list(&cli),
        Commands::Remove { id, purge_history } => cmd_remove(&cli, id, *purge_history),
        Commands::Preview(args) => cmd_preview(&cli, args).await,
        Commands::Check(args) => cmd_check(&cli, args).await,
        Commands::Diff { id, context } => cmd_diff(&cli, id, *context),
        Commands::History { id, limit } => cmd_history(&cli, id, *limit),
        Commands::Watch { concurrency } => cmd_watch(&cli, *concurrency).await,
        Commands::Mcp => {
            webdelta_mcp::run_stdio(cli.config, cli.db).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn cmd_init(cli: &Cli, force: bool) -> Result<ExitCode> {
    if cli.config.exists() && !force {
        return Err(Error::Config(format!(
            "config file '{}' already exists (use --force to overwrite)",
            cli.config.display()
        )));
    }
    if let Some(parent) = cli.config.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let content = starter_config_toml();
    fs::write(&cli.config, &content)?;
    let parsed = load_config(&cli.config)?;

    if cli.json {
        print_json(&json!({
            "created": cli.config.display().to_string(),
            "watches": parsed.watches.len(),
        }));
    } else {
        println!("Created {}", cli.config.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_validate(cli: &Cli) -> Result<ExitCode> {
    let config = load_config(&cli.config)?;
    let enabled_count = config.watches.iter().filter(|w| w.enabled).count();
    if cli.json {
        print_json(&json!({
            "valid": true,
            "config": cli.config.display().to_string(),
            "watches": config.watches.len(),
            "enabled_watches": enabled_count,
            "notifiers": config.notifiers.len(),
        }));
    } else {
        println!(
            "Valid configuration: {} watch(es) ({} enabled), {} notifier(s)",
            config.watches.len(),
            enabled_count,
            config.notifiers.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_add(cli: &Cli, args: &AddArgs) -> Result<ExitCode> {
    let mut config = if cli.config.exists() {
        load_config(&cli.config)?
    } else {
        Config::default()
    };

    if config.watches.iter().any(|w| w.id == args.id) {
        return Err(Error::Validation(format!(
            "watch '{}' already exists in {}",
            args.id,
            cli.config.display()
        )));
    }

    let extractor = build_extractor_from_cli(&args.extractor)?;
    let mut conditions = Vec::new();
    for text in &args.contains {
        conditions.push(ConditionConfig::Contains {
            value: text.clone(),
            case_sensitive: false,
        });
    }
    for text in &args.not_contains {
        conditions.push(ConditionConfig::NotContains {
            value: text.clone(),
            case_sensitive: false,
        });
    }
    if let Some(threshold) = args.lt {
        conditions.push(ConditionConfig::NumericLt { threshold });
    }
    if let Some(threshold) = args.gt {
        conditions.push(ConditionConfig::NumericGt { threshold });
    }
    if let Some(percent) = args.drop_pct {
        conditions.push(ConditionConfig::DropPctAtLeast { percent });
    }
    if let Some(percent) = args.rise_pct {
        conditions.push(ConditionConfig::RisePctAtLeast { percent });
    }

    let watch = WatchConfig {
        id: args.id.clone(),
        name: args.name.clone(),
        url: args.url.clone(),
        method: HttpMethod::Get,
        headers: BTreeMap::new(),
        interval: args.interval.clone(),
        timeout_secs: None,
        retries: None,
        engine: args.browser.engine.into(),
        browser: args.browser.to_browser_config(),
        extractor,
        ignore_regex: args.extractor.ignore_regex.clone(),
        normalize: NormalizeConfig::default(),
        conditions,
        notifiers: args.notifiers.clone(),
        enabled: true,
    };

    config.watches.push(watch.clone());
    save_config(&cli.config, &config)?;

    if cli.json {
        print_json(&json!({
            "added": true,
            "config": cli.config.display().to_string(),
            "watch": watch,
        }));
    } else {
        println!("Added watch '{}' -> {}", watch.id, watch.url);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_list(cli: &Cli) -> Result<ExitCode> {
    let config = load_config(&cli.config)?;
    let store = if cli.db.exists() {
        Some(StateStore::open(&cli.db)?)
    } else {
        None
    };

    let mut rows = Vec::with_capacity(config.watches.len());
    for watch in &config.watches {
        let latest = match &store {
            Some(st) => st.get_latest(&watch.id)?,
            None => None,
        };
        rows.push((watch, latest));
    }

    if cli.json {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|(w, latest)| {
                json!({
                    "id": w.id,
                    "name": w.display_name(),
                    "url": w.url,
                    "interval": w.interval.as_deref().unwrap_or(&config.defaults.interval),
                    "enabled": w.enabled,
                    "engine": w.engine,
                    "extractor": w.extractor,
                    "latest": latest,
                })
            })
            .collect();
        print_json(&json!({ "watches": items }));
        return Ok(ExitCode::SUCCESS);
    }

    if rows.is_empty() {
        println!("No watches configured in {}", cli.config.display());
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{:<18} {:<8} {:<8} {:<20} {:<12} LATEST VALUE",
        "ID", "INT", "STATE", "LAST CHECK (UTC)", "NUMERIC"
    );
    for (watch, latest) in rows {
        let interval = watch
            .interval
            .as_deref()
            .unwrap_or(&config.defaults.interval);
        let state = if watch.enabled { "enabled" } else { "paused" };
        match latest {
            Some(snap) => {
                let checked = snap.checked_at.format("%Y-%m-%d %H:%M:%S").to_string();
                let num = snap
                    .numeric_value
                    .map(format_number)
                    .unwrap_or_else(|| "-".to_string());
                let snippet = one_line_snippet(&snap.extracted_text, 48);
                println!(
                    "{:<18} {:<8} {:<8} {:<20} {:<12} {}",
                    truncate_cell(&watch.id, 18),
                    interval,
                    state,
                    checked,
                    num,
                    snippet
                );
            }
            None => {
                println!(
                    "{:<18} {:<8} {:<8} {:<20} {:<12} (no snapshot)",
                    truncate_cell(&watch.id, 18),
                    interval,
                    state,
                    "never",
                    "-"
                );
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_remove(cli: &Cli, id: &str, purge_history: bool) -> Result<ExitCode> {
    let mut config = load_config(&cli.config)?;
    let before = config.watches.len();
    config.watches.retain(|w| w.id != id);
    if config.watches.len() == before {
        return Err(Error::Validation(format!(
            "watch '{id}' not found in {}",
            cli.config.display()
        )));
    }
    save_config(&cli.config, &config)?;

    let purged = if purge_history && cli.db.exists() {
        let store = StateStore::open(&cli.db)?;
        store.purge_watch(id)?
    } else {
        0
    };

    if cli.json {
        print_json(&json!({
            "removed": id,
            "purged_snapshots": purged,
        }));
    } else if purge_history {
        println!("Removed watch '{id}' and purged {purged} stored record(s)");
    } else {
        println!("Removed watch '{id}'");
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_preview(cli: &Cli, args: &PreviewArgs) -> Result<ExitCode> {
    let extractor = build_extractor_from_cli(&args.extractor)?;
    let headers = parse_header_pairs(&args.request_headers)?;
    let browser = args.browser.to_browser_config();
    let preview = MonitorEngine::preview(
        &args.url,
        &extractor,
        &args.extractor.ignore_regex,
        &NormalizeConfig::default(),
        args.timeout,
        args.browser.engine.into(),
        &browser,
        &headers,
    )
    .await?;

    if cli.json {
        print_json(&preview);
    } else {
        println!("URL: {}", preview.url);
        println!(
            "HTTP: {} ({}, {} ms)",
            preview.status_code, preview.engine_used, preview.elapsed_ms
        );
        if let Some(num) = preview.numeric_value {
            println!("Numeric: {}", format_number(num));
        }
        println!("SHA-256: {}", &preview.content_hash[..12]);
        println!("Extracted:\n{}", preview.extracted_text);
    }
    Ok(ExitCode::SUCCESS)
}

async fn cmd_check(cli: &Cli, args: &CheckArgs) -> Result<ExitCode> {
    let config = load_config(&cli.config)?;
    let store = StateStore::open(&cli.db)?;
    let engine = MonitorEngine::new(store)?;

    let concurrency = args.concurrency.unwrap_or(config.defaults.concurrency);
    let filter = if args.ids.is_empty() {
        None
    } else {
        Some(args.ids.as_slice())
    };

    let outcomes = engine
        .check_watches(&config, filter, concurrency, !args.no_notify)
        .await?;

    let has_errors = outcomes.iter().any(|o| o.status == CheckStatus::Error);
    let has_changes = outcomes.iter().any(|o| o.status == CheckStatus::Changed);

    if cli.json {
        print_json(&json!({
            "count": outcomes.len(),
            "changed": outcomes.iter().filter(|o| o.status == CheckStatus::Changed).count(),
            "errors": outcomes.iter().filter(|o| o.status == CheckStatus::Error).count(),
            "results": outcomes,
        }));
    } else if outcomes.is_empty() {
        println!("No enabled watches to check.");
    } else {
        for outcome in &outcomes {
            print_outcome_human(outcome);
        }
    }

    if has_errors {
        Ok(ExitCode::from(1))
    } else if args.exit_code && has_changes {
        Ok(ExitCode::from(2))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn cmd_diff(cli: &Cli, id: &str, context: usize) -> Result<ExitCode> {
    if !cli.db.exists() {
        return Err(Error::Storage(format!(
            "state database '{}' does not exist yet (run 'webdelta check' first)",
            cli.db.display()
        )));
    }
    let store = StateStore::open(&cli.db)?;
    let Some((prev, curr)) = store.get_latest_pair(id)? else {
        if cli.json {
            print_json(&json!({
                "watch_id": id,
                "has_diff": false,
                "message": "fewer than two snapshots recorded"
            }));
        } else {
            println!("Fewer than two snapshots recorded for '{id}'.");
        }
        return Ok(ExitCode::SUCCESS);
    };

    let diff = compute_diff(&prev.extracted_text, &curr.extracted_text, context);
    let numeric_delta = match (prev.numeric_value, curr.numeric_value) {
        (Some(p), Some(c)) => Some(compute_delta(p, c)),
        _ => None,
    };

    if cli.json {
        print_json(&json!({
            "watch_id": id,
            "has_diff": prev.extracted_text != curr.extracted_text,
            "previous": prev,
            "current": curr,
            "numeric_delta": numeric_delta,
            "diff": diff,
        }));
    } else {
        println!(
            "Watch: {id}\nPrevious ({}): {}\nCurrent  ({}): {}",
            prev.checked_at.format("%Y-%m-%d %H:%M:%S UTC"),
            one_line_snippet(&prev.extracted_text, 80),
            curr.checked_at.format("%Y-%m-%d %H:%M:%S UTC"),
            one_line_snippet(&curr.extracted_text, 80),
        );
        if let Some(delta) = &numeric_delta {
            if let Some(pct) = delta.percent_change {
                println!("Numeric delta: {:+.2} ({:+.2}%)", delta.diff, pct);
            } else {
                println!("Numeric delta: {:+.2}", delta.diff);
            }
        }
        if prev.extracted_text == curr.extracted_text {
            println!("No changes between the two most recent snapshots.");
        } else {
            println!("Summary: {}\n{}", diff.summary, diff.unified);
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_history(cli: &Cli, id: &str, limit: usize) -> Result<ExitCode> {
    if !cli.db.exists() {
        if cli.json {
            print_json(&json!({ "watch_id": id, "count": 0, "snapshots": [] }));
        } else {
            println!("No history recorded for '{id}'.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let store = StateStore::open(&cli.db)?;
    let records = store.get_history(id, limit)?;

    if cli.json {
        print_json(&json!({
            "watch_id": id,
            "count": records.len(),
            "snapshots": records,
        }));
        return Ok(ExitCode::SUCCESS);
    }

    if records.is_empty() {
        println!("No history recorded for '{id}'.");
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{:<22} {:<6} {:<8} {:<12} {:<10} VALUE",
        "CHECKED AT (UTC)", "HTTP", "ELAPSED", "NUMERIC", "HASH"
    );
    for rec in records {
        let ts = rec.checked_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let num = rec
            .numeric_value
            .map(format_number)
            .unwrap_or_else(|| "-".to_string());
        let hash_prefix = &rec.content_hash[..rec.content_hash.len().min(8)];
        let snippet = one_line_snippet(&rec.extracted_text, 48);
        println!(
            "{:<22} {:<6} {:<8} {:<12} {:<10} {}",
            ts,
            rec.status_code,
            format!("{}ms", rec.elapsed_ms),
            num,
            hash_prefix,
            snippet
        );
    }

    Ok(ExitCode::SUCCESS)
}

async fn cmd_watch(cli: &Cli, concurrency_override: Option<usize>) -> Result<ExitCode> {
    let config = load_config(&cli.config)?;
    let enabled_watches: Vec<WatchConfig> = config
        .watches
        .iter()
        .filter(|w| w.enabled)
        .cloned()
        .collect();
    if enabled_watches.is_empty() {
        return Err(Error::Config(
            "no enabled watches configured; add watches or enable one before running 'watch'"
                .to_string(),
        ));
    }

    let mut schedules: Vec<(String, Duration, Instant)> = Vec::with_capacity(enabled_watches.len());
    let now = Instant::now();
    for watch in &enabled_watches {
        let interval = watch.effective_interval(&config.defaults)?;
        schedules.push((watch.id.clone(), interval, now));
    }

    let store = StateStore::open(&cli.db)?;
    let engine = MonitorEngine::new(store)?;
    let concurrency = concurrency_override.unwrap_or(config.defaults.concurrency);

    if !cli.json {
        println!(
            "Watching {} target(s) from {} (press Ctrl+C to stop)",
            schedules.len(),
            cli.config.display()
        );
    }

    loop {
        let current = Instant::now();
        let mut due_ids = Vec::new();
        for (id, interval, next_due) in &mut schedules {
            if *next_due <= current {
                due_ids.push(id.clone());
                *next_due = current + *interval;
            }
        }

        if !due_ids.is_empty() {
            let outcomes = engine
                .check_watches(&config, Some(&due_ids), concurrency, true)
                .await?;
            for outcome in &outcomes {
                if cli.json {
                    print_json(outcome);
                } else {
                    print_outcome_human(outcome);
                }
            }
        }

        let earliest = schedules
            .iter()
            .map(|(_, _, next_due)| *next_due)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(60));
        let wait_duration = earliest.saturating_duration_since(Instant::now());

        tokio::select! {
            _ = tokio::time::sleep(wait_duration) => {}
            ctrl = tokio::signal::ctrl_c() => {
                if let Err(err) = ctrl {
                    return Err(Error::Io(err));
                }
                if !cli.json {
                    println!("Shutdown signal received, stopping scheduler.");
                }
                break;
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn parse_header_pairs(raw: &[String]) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for item in raw {
        let Some((key, val)) = item.split_once(':') else {
            return Err(Error::Validation(format!(
                "invalid header '{item}': expected 'Key: Value'"
            )));
        };
        let key = key.trim();
        let val = val.trim();
        if key.is_empty() {
            return Err(Error::Validation(format!(
                "invalid header '{item}': header name must not be empty"
            )));
        }
        headers.insert(key.to_string(), val.to_string());
    }
    Ok(headers)
}

fn build_extractor_from_cli(args: &ExtractorCliArgs) -> Result<ExtractorConfig> {
    if let Some(selector) = &args.css {
        return Ok(ExtractorConfig::Css {
            selector: selector.clone(),
            attribute: args.attr.clone(),
            all: args.all,
        });
    }
    if let Some(path) = &args.jsonpath {
        return Ok(ExtractorConfig::JsonPath { path: path.clone() });
    }
    if let Some(pattern) = &args.regex {
        return Ok(ExtractorConfig::Regex {
            pattern: pattern.clone(),
            group: args.group,
        });
    }
    if args.status {
        return Ok(ExtractorConfig::Status {
            header: args.header.clone(),
        });
    }
    Ok(ExtractorConfig::Text)
}

fn print_outcome_human(outcome: &CheckOutcome) {
    let status_label = outcome.status.as_str().to_uppercase();
    let code_label = outcome
        .status_code
        .map(|c| format!("HTTP {c}"))
        .unwrap_or_else(|| "no HTTP".to_string());
    let engine_suffix = if outcome.engine_used == "browser" {
        ", browser"
    } else {
        ""
    };

    println!(
        "[{}] {} ({}{}, {}ms)",
        status_label, outcome.watch_id, code_label, engine_suffix, outcome.elapsed_ms
    );

    if let Some(err) = &outcome.error {
        println!("  error: {err}");
        return;
    }

    match outcome.status {
        CheckStatus::Initial => {
            if let Some(val) = &outcome.current_value {
                println!("  baseline: {}", one_line_snippet(val, 100));
            }
        }
        CheckStatus::Changed | CheckStatus::RuleNotMet => {
            if let Some(prev) = &outcome.previous_value {
                println!("  previous: {}", one_line_snippet(prev, 100));
            }
            if let Some(curr) = &outcome.current_value {
                println!("  current : {}", one_line_snippet(curr, 100));
            }
            if let Some(delta) = &outcome.numeric_delta {
                if let Some(pct) = delta.percent_change {
                    println!("  delta   : {:+.2} ({:+.2}%)", delta.diff, pct);
                } else {
                    println!("  delta   : {:+.2}", delta.diff);
                }
            }
            if !outcome.matched_rules.is_empty() {
                println!("  rules   : {}", outcome.matched_rules.join(", "));
            }
            if let Some(diff) = &outcome.diff {
                if !diff.unified.trim().is_empty() {
                    for line in diff.unified.lines() {
                        println!("  {line}");
                    }
                }
            }
            for notif in &outcome.notifications {
                if notif.ok {
                    println!(
                        "  notified: {} ({}, HTTP {})",
                        notif.notifier,
                        notif.kind,
                        notif.status_code.unwrap_or(200)
                    );
                } else {
                    println!(
                        "  notify failed: {} ({}): {}",
                        notif.notifier,
                        notif.kind,
                        notif.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
        }
        CheckStatus::Unchanged | CheckStatus::NotModified => {
            if let Some(val) = &outcome.current_value {
                println!("  value   : {}", one_line_snippet(val, 100));
            }
        }
        CheckStatus::Error => {}
    }
}

fn print_json<T: serde::Serialize>(value: &T) {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    println!("{text}");
}

fn one_line_snippet(input: &str, max_chars: usize) -> String {
    let collapsed: String = input.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_cell(&collapsed, max_chars)
}

fn truncate_cell(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let prefix: String = input.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{prefix}...")
}

fn format_number(val: f64) -> String {
    if (val - val.round()).abs() < 1e-9 {
        format!("{}", val.round() as i64)
    } else {
        format!("{val:.2}")
    }
}

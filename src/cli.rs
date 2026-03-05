use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::pipeline;

#[derive(Parser)]
#[command(name = "pail", about = "Personal AI Lurker — AI-powered digest generation")]
pub struct Cli {
    /// Path to configuration file
    #[arg(long, short, global = true, default_value = "config.toml")]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Config file management (validate, edit sources)
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Generate a digest article for an output channel
    Generate {
        /// Output channel slug
        slug: String,

        /// Write raw markdown output to this file
        #[arg(long)]
        output: Option<PathBuf>,

        /// Override generation strategy (default: channel's configured strategy)
        #[arg(long)]
        strategy: Option<String>,

        /// Override time window with relative duration (e.g., "7d", "12h"). Mutually exclusive with --from/--to.
        #[arg(long, conflicts_with_all = ["from", "to"])]
        since: Option<String>,

        /// Exact start of time window (RFC 3339, e.g., "2026-02-14T20:00:00Z"). Requires --to.
        #[arg(long, requires = "to")]
        from: Option<String>,

        /// Exact end of time window (RFC 3339, e.g., "2026-02-16T08:00:00Z"). Requires --from.
        #[arg(long, requires = "from")]
        to: Option<String>,
    },

    /// Launch an interactive opencode TUI session with collected source data
    Interactive {
        /// Output channel slug
        slug: String,

        /// Override generation strategy (default: channel's configured strategy)
        #[arg(long)]
        strategy: Option<String>,

        /// Override time window with relative duration (e.g., "7d", "12h"). Mutually exclusive with --from/--to.
        #[arg(long, conflicts_with_all = ["from", "to"])]
        since: Option<String>,

        /// Exact start of time window (RFC 3339, e.g., "2026-02-14T20:00:00Z"). Requires --to.
        #[arg(long, requires = "to")]
        from: Option<String>,

        /// Exact end of time window (RFC 3339, e.g., "2026-02-16T08:00:00Z"). Requires --from.
        #[arg(long, requires = "from")]
        to: Option<String>,
    },

    /// Run benchmarks for article generation
    Benchmark {
        #[command(subcommand)]
        command: BenchmarkCommands,
    },

    /// Inspect and validate generation strategies
    Strategy {
        #[command(subcommand)]
        command: StrategyCommands,
    },

    /// Telegram session management
    Tg {
        #[command(subcommand)]
        command: TgCommands,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Validate the configuration file
    Validate,
    /// Interactive TUI for managing Telegram sources
    Edit,
}

#[derive(Subcommand)]
pub enum BenchmarkCommands {
    /// Run all models and collect article outputs
    Run {
        /// Override time window (e.g., "7d", "12h")
        #[arg(long, conflicts_with_all = ["from", "to"])]
        since: Option<String>,

        /// Exact start of time window (RFC 3339)
        #[arg(long, requires = "to")]
        from: Option<String>,

        /// Exact end of time window (RFC 3339)
        #[arg(long, requires = "from")]
        to: Option<String>,

        /// Output channel slug (default: first in config)
        #[arg(long)]
        channel: Option<String>,

        /// Generation strategy override (default: channel's configured strategy)
        #[arg(long)]
        strategy: Option<String>,

        /// Samples per model (default: 5)
        #[arg(long, default_value = "5")]
        samples: usize,

        /// Delay between samples of the same model (default: "5s")
        #[arg(long, default_value = "5s")]
        delay: String,

        /// Per-generation timeout (default: strategy's timeout)
        #[arg(long)]
        timeout: Option<String>,

        /// Comma-separated model IDs (default: auto-discover opencode/*)
        #[arg(long)]
        models: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum StrategyCommands {
    /// List all available strategies (built-in + user-defined)
    List,
    /// Show a strategy's resolved config
    Show {
        /// Strategy name
        name: String,
    },
    /// Validate a user strategy directory
    Validate {
        /// Path to strategy directory
        path: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
pub enum TgCommands {
    /// Interactive MTProto login wizard
    Login,
    /// Show Telegram session status
    Status,
}

/// Parse --since/--from/--to into a TimeWindow.
pub fn parse_time_window(
    since: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
) -> Result<Option<pipeline::TimeWindow>> {
    if let Some(since_str) = since {
        let duration =
            humantime::parse_duration(since_str).with_context(|| format!("invalid --since duration: '{since_str}'"))?;
        Ok(Some(pipeline::TimeWindow::Since(duration)))
    } else if let (Some(from_str), Some(to_str)) = (from, to) {
        let from_dt = chrono::DateTime::parse_from_rfc3339(from_str)
            .with_context(|| format!("invalid --from timestamp: '{from_str}' (expected RFC 3339)"))?
            .to_utc();
        let to_dt = chrono::DateTime::parse_from_rfc3339(to_str)
            .with_context(|| format!("invalid --to timestamp: '{to_str}' (expected RFC 3339)"))?
            .to_utc();
        if from_dt >= to_dt {
            anyhow::bail!("--from must be before --to");
        }
        Ok(Some(pipeline::TimeWindow::Explicit {
            from: from_dt,
            to: to_dt,
        }))
    } else {
        Ok(None)
    }
}

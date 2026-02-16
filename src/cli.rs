use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    /// Validate the configuration file
    Validate,

    /// Generate a digest article for an output channel
    Generate {
        /// Output channel slug
        slug: String,

        /// Write raw markdown output to this file
        #[arg(long)]
        output: Option<PathBuf>,

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

    /// Telegram session management
    Tg {
        #[command(subcommand)]
        command: TgCommands,
    },
}

#[derive(Subcommand)]
pub enum TgCommands {
    /// Interactive MTProto login wizard
    Login,
    /// Show Telegram session status
    Status,
}

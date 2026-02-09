use anyhow::Result;
use clap::Parser;
use lib::Config;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "sybil")]
#[command(about = "S3-native log storage with SQL query interface")]
#[command(version)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Run as indexer (accepts logs, writes segments)
    #[arg(long, group = "mode")]
    indexer: bool,

    /// Run as querier (executes SQL queries)
    #[arg(long, group = "mode")]
    querier: bool,

    /// Run as steward (background housekeeping)
    #[arg(long, group = "mode")]
    steward: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Indexer,
    Querier,
    Steward,
}

impl Cli {
    fn mode(&self) -> Option<Mode> {
        if self.indexer {
            Some(Mode::Indexer)
        } else if self.querier {
            Some(Mode::Querier)
        } else if self.steward {
            Some(Mode::Steward)
        } else {
            None
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config = if cli.config.exists() {
        Config::from_file(&cli.config)?
    } else {
        tracing::warn!(path = ?cli.config, "config file not found, using defaults");
        Config::default()
    };

    let mode = cli.mode().ok_or_else(|| {
        anyhow::anyhow!("must specify a mode: --indexer, --querier, or --steward")
    })?;

    tracing::info!(?mode, "starting sybil");

    match mode {
        Mode::Indexer => {
            tracing::info!(
                host = %config.server.host,
                port = %config.server.port,
                "starting indexer"
            );
            indexer::run(config).await?;
        }
        Mode::Querier => {
            tracing::info!(
                host = %config.server.host,
                port = %config.server.port,
                "starting querier"
            );
            querier::run(config).await?;
        }
        Mode::Steward => {
            tracing::info!("starting steward");
            steward::run(config).await?;
        }
    }

    Ok(())
}

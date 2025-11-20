use anyhow::Result;
use tracing_subscriber::filter::LevelFilter;

mod data_types;
mod cli;
mod cli_commands;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing to see network events and output
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .with_test_writer()
        .init();

    cli::run().await
}

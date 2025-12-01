//! FAT Filesystem CLI Tool - Main Entry Point

use anyhow::Result;
use clap::Parser;

mod block_device;
mod cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    cli::run(cli).await
}

use anyhow::Result;
use clap::{Parser, Subcommand};
use crate::cli_commands;

#[derive(Parser, Debug)]
#[command(author, version, about = "Amaru Debug Tools CLI for Cardano networking diagnostics.")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Network diagnostic tools.
    Network(NetworkCommand),
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Network diagnostic tools.")]
pub struct NetworkCommand {
    #[command(subcommand)]
    pub command: NetworkSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum NetworkSubcommand {
    /// Synchronizes headers from two relays starting at a specific block and reports the first divergence point.
    SlotDivergence(SlotDivergenceArgs),

    /// Queries a relay to determine the highest supported Ouroboros network protocol version.
    ProtocolVersion(ProtocolVersionArgs),
}

#[derive(Parser, Debug)]
pub struct SlotDivergenceArgs {
    /// Address and port of the first relay (e.g., node.example.com:3001).
    #[arg(long)]
    pub relay_a: String,

    /// Address and port of the second relay (e.g., node2.example.com:3001).
    #[arg(long)]
    pub relay_b: String,

    /// Absolute slot number to begin the divergence check from.
    #[arg(long)]
    pub slot: u64,

    /// Hash of the block at the starting slot (hex encoded).
    #[arg(long)]
    pub hash: String,
}

#[derive(Parser, Debug)]
pub struct ProtocolVersionArgs {
    /// Address and port of the target relay (e.g., node.example.com:3001).
    #[arg(long)]
    pub relay: String,

    /// The network magic number (e.g., 764824073 for Mainnet, 2 for Preview).
    #[arg(long, default_value_t = 764824073)]
    pub magic: u64,
}


pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Network(network_cmd) => match network_cmd.command {
            // Updated to include the new command
            NetworkSubcommand::SlotDivergence(args) => cli_commands::run_slot_divergence(args).await,
            NetworkSubcommand::ProtocolVersion(args) => cli_commands::run_protocol_version(args).await,
        },
    }
}

//! `xslot-cli` — off-chain analysis tool for the cross-slot guard.
//!
//! Subcommands:
//! - `analyze` — pull real swaps for a pool from Helius and replay them through
//!   the guard, reporting how many would be rejected and the CU overhead.
//! - `simulate` — run a synthetic cross-slot sandwich against the guard to
//!   demonstrate detection without needing network access.
//! - `replay` — replay a local JSON file of labelled swaps and print a full
//!   detection scorecard (precision/recall/FPR).

mod cu;
mod helius;
mod replay;
mod types;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use xslot_core::GuardConfig;

use crate::helius::HeliusClient;
use crate::replay::replay;
use crate::types::{AnalysisReport, SwapEvent};

#[derive(Parser)]
#[command(
    name = "xslot-cli",
    about = "Cross-slot sandwich MEV detection analysis for Solana",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Deviation tolerance in basis points (a swap beyond this from the TWAP is
    /// rejected). 100 bps == 1%.
    #[arg(long, global = true, default_value_t = 100)]
    tolerance_bps: u64,

    /// Minimum observations before the guard begins enforcing.
    #[arg(long, global = true, default_value_t = 8)]
    min_observations: usize,

    /// Emit the report as JSON instead of a human table.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch real swaps from Helius and replay them through the guard.
    Analyze {
        /// Pool or wallet address to pull transactions for.
        #[arg(long)]
        address: String,
        /// Base token mint.
        #[arg(long)]
        base_mint: String,
        /// Quote token mint.
        #[arg(long)]
        quote_mint: String,
        /// Number of transactions to fetch (max 100 per Helius call).
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Helius API key. Falls back to the HELIUS_API_KEY env var.
        #[arg(long, env = "HELIUS_API_KEY")]
        api_key: String,
    },
    /// Run a synthetic cross-slot sandwich scenario (no network needed).
    Simulate {
        /// Honest baseline price.
        #[arg(long, default_value_t = 100.0)]
        baseline: f64,
        /// Manipulated price the attacker pushes the pool to.
        #[arg(long, default_value_t = 130.0)]
        manipulated: f64,
        /// Number of honest swaps before the attack.
        #[arg(long, default_value_t = 20)]
        honest_swaps: u64,
    },
    /// Replay a local JSON array of SwapEvents (optionally labelled).
    Replay {
        /// Path to a JSON file containing an array of SwapEvent objects.
        #[arg(long)]
        path: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = GuardConfig {
        tolerance_bps: cli.tolerance_bps,
        min_observations: cli.min_observations,
    };

    let report = match &cli.command {
        Command::Analyze {
            address,
            base_mint,
            quote_mint,
            limit,
            api_key,
        } => {
            let client = HeliusClient::new(api_key.clone());
            let events = client
                .fetch_swaps(address, base_mint, quote_mint, *limit)
                .context("fetching swaps from Helius")?;
            eprintln!("Fetched {} swap events from Helius.", events.len());
            replay(&events, config)?
        }
        Command::Simulate {
            baseline,
            manipulated,
            honest_swaps,
        } => {
            let events = synthetic_sandwich(*baseline, *manipulated, *honest_swaps);
            replay(&events, config)?
        }
        Command::Replay { path } => {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("reading {path}"))?;
            let events: Vec<SwapEvent> =
                serde_json::from_str(&data).context("parsing swap JSON")?;
            replay(&events, config)?
        }
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report, &config);
    }
    Ok(())
}

/// Build a synthetic labelled cross-slot sandwich:
/// `honest_swaps` honest trades at `baseline`, spaced several slots apart, then
/// one manipulated trade at `manipulated`.
fn synthetic_sandwich(baseline: f64, manipulated: f64, honest_swaps: u64) -> Vec<SwapEvent> {
    let mut events = Vec::new();
    let mut slot = 1u64;
    for i in 0..honest_swaps {
        events.push(SwapEvent {
            slot,
            signature: format!("honest-{i}"),
            price: baseline,
            is_attack: Some(false),
        });
        slot += 2; // honest swaps a couple slots apart
    }
    // The attack lands a few slots after the last honest swap.
    events.push(SwapEvent {
        slot: slot + 3,
        signature: "attack".into(),
        price: manipulated,
        is_attack: Some(true),
    });
    events
}

fn print_report(report: &AnalysisReport, config: &GuardConfig) {
    println!("\n  Cross-Slot Guard — Analysis Report");
    println!("  ----------------------------------");
    println!("  tolerance           : {} bps", config.tolerance_bps);
    println!("  min observations    : {}", config.min_observations);
    println!();
    println!("  evaluated swaps     : {}", report.evaluated);
    println!("  allowed             : {}", report.allowed);
    println!("  rejected            : {}", report.rejected);
    println!("  warmup skipped      : {}", report.warmup_skipped);
    println!("  CU overhead / swap  : ~{}", report.cu_overhead_per_swap);

    if report.evaluated > 0 {
        let pct = 100.0 * report.rejected as f64 / report.evaluated as f64;
        println!("  rejection rate      : {pct:.2}%");
    }

    if let Some(score) = &report.labelled {
        println!();
        println!("  Labelled detection scorecard");
        println!("  ----------------------------");
        println!("  true positives      : {}", score.true_positives);
        println!("  false positives     : {}", score.false_positives);
        println!("  false negatives     : {}", score.false_negatives);
        println!("  true negatives      : {}", score.true_negatives);
        println!("  detection rate      : {:.2}%", 100.0 * score.detection_rate());
        println!("  precision           : {:.2}%", 100.0 * score.precision());
        println!(
            "  false positive rate : {:.2}%",
            100.0 * score.false_positive_rate()
        );
    }
    println!();
}

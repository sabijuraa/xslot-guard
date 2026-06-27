//! Minimal Helius client for pulling real swap history from Solana mainnet.
//!
//! We deliberately use only two endpoints and a blocking HTTP client (`ureq`)
//! to keep the dependency surface tiny — this is an analysis tool, not a
//! latency-sensitive service. The methods return already-parsed
//! [`SwapEvent`]s sorted by slot, ready for the replay engine.
//!
//! The price reconstruction here is intentionally simple: we read the parsed
//! token transfers Helius attaches to a swap and compute price as
//! `quote_amount / base_amount`. For a production indexer you would decode the
//! pool's `sqrtPriceX64` from the post-swap account state; that refinement is
//! documented in `docs/price-reconstruction.md` and does not change the guard
//! logic, only the precision of the input.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::types::SwapEvent;

/// A thin wrapper over the Helius enhanced-transactions API.
pub struct HeliusClient {
    api_key: String,
    base: String,
}

#[derive(Debug, Deserialize)]
struct EnhancedTx {
    signature: String,
    slot: u64,
    #[serde(default, rename = "tokenTransfers")]
    token_transfers: Vec<TokenTransfer>,
}

#[derive(Debug, Deserialize)]
struct TokenTransfer {
    #[serde(rename = "tokenAmount")]
    token_amount: f64,
    #[serde(default, rename = "mint")]
    mint: String,
}

impl HeliusClient {
    /// Create a client from a raw API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base: "https://api.helius.xyz".to_string(),
        }
    }

    /// Override the base URL (used by tests against a local mock).
    #[allow(dead_code)]
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    /// Fetch up to `limit` recent parsed transactions for `address` and
    /// reconstruct swap events from them.
    ///
    /// `base_mint` and `quote_mint` identify which two token transfers define
    /// the price; transactions that do not contain both are skipped (they are
    /// not swaps on this pair).
    pub fn fetch_swaps(
        &self,
        address: &str,
        base_mint: &str,
        quote_mint: &str,
        limit: usize,
    ) -> Result<Vec<SwapEvent>> {
        let url = format!(
            "{}/v0/addresses/{}/transactions?api-key={}&limit={}",
            self.base, address, self.api_key, limit
        );

        let resp = ureq::get(&url)
            .call()
            .context("Helius request failed (check API key and network)")?;
        let txs: Vec<EnhancedTx> = resp
            .into_json()
            .context("failed to parse Helius response as enhanced transactions")?;

        let mut events = Vec::new();
        for tx in txs {
            if let Some(price) = reconstruct_price(&tx, base_mint, quote_mint) {
                events.push(SwapEvent {
                    slot: tx.slot,
                    signature: tx.signature,
                    price,
                    is_attack: None,
                });
            }
        }

        // Helius returns newest-first; the replay engine needs oldest-first.
        events.sort_by_key(|e| e.slot);
        Ok(events)
    }
}

/// Reconstruct an execution price from a parsed transaction's token transfers.
///
/// Returns `None` if the transaction does not move both the base and quote
/// mints (i.e. it is not a swap on this pair).
fn reconstruct_price(tx: &EnhancedTx, base_mint: &str, quote_mint: &str) -> Option<f64> {
    let mut base_amount = 0.0;
    let mut quote_amount = 0.0;
    for tr in &tx.token_transfers {
        if tr.mint == base_mint {
            base_amount += tr.token_amount.abs();
        } else if tr.mint == quote_mint {
            quote_amount += tr.token_amount.abs();
        }
    }
    if base_amount > 0.0 && quote_amount > 0.0 {
        Some(quote_amount / base_amount)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_reconstruction_basic() {
        let tx = EnhancedTx {
            signature: "x".into(),
            slot: 10,
            token_transfers: vec![
                TokenTransfer {
                    token_amount: 2.0,
                    mint: "BASE".into(),
                },
                TokenTransfer {
                    token_amount: 300.0,
                    mint: "QUOTE".into(),
                },
            ],
        };
        let p = reconstruct_price(&tx, "BASE", "QUOTE").unwrap();
        assert_eq!(p, 150.0);
    }

    #[test]
    fn skips_non_pair_transactions() {
        let tx = EnhancedTx {
            signature: "x".into(),
            slot: 10,
            token_transfers: vec![TokenTransfer {
                token_amount: 2.0,
                mint: "OTHER".into(),
            }],
        };
        assert!(reconstruct_price(&tx, "BASE", "QUOTE").is_none());
    }
}

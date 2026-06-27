//! Replay engine: feed a chronological stream of [`SwapEvent`]s through the
//! cross-slot guard and produce an [`AnalysisReport`].
//!
//! This is the bridge between off-chain data and the on-chain algorithm. Every
//! price is converted into the same [`FixedPrice`] the program uses, so the
//! detection numbers we report are the numbers the deployed guard would
//! produce on identical input.

use anyhow::{Context, Result};
use xslot_core::{CrossSlotGuard, FixedPrice, GuardConfig, GuardDecision, SlotTwapOracle};

use crate::cu;
use crate::types::{AnalysisReport, LabelledScore, SwapEvent};

/// Number of decimal places we preserve when converting an `f64` price into the
/// integer fixed-point domain before scaling. Nine matches `SCALE`'s precision.
const PRICE_DECIMALS: u32 = 6;

/// Convert a positive float price into the core's fixed-point representation.
///
/// We multiply by `10^PRICE_DECIMALS`, round, and hand the integer to
/// `FixedPrice::from_scaled`. Prices that are non-finite or non-positive are an
/// error — they indicate corrupt input, not a tradeable price.
fn to_fixed(price: f64) -> Result<FixedPrice> {
    anyhow::ensure!(price.is_finite() && price > 0.0, "invalid price: {price}");
    let scaled = (price * 10f64.powi(PRICE_DECIMALS as i32)).round() as u64;
    FixedPrice::from_scaled(scaled, PRICE_DECIMALS)
        .map_err(|e| anyhow::anyhow!("price {price} failed fixed-point conversion: {e:?}"))
}

/// Replay `events` (which must be sorted by slot ascending) through a guard
/// configured with `config`.
///
/// For each event we:
/// 1. ask the guard whether the swap price is consistent with the TWAP,
/// 2. record the decision, and
/// 3. feed the realized price back into the oracle as a new observation.
///
/// Step 3 mirrors on-chain reality: after a swap executes, the pool's price is
/// updated, and the next swap sees that price in history.
pub fn replay(events: &[SwapEvent], config: GuardConfig) -> Result<AnalysisReport> {
    let guard =
        CrossSlotGuard::new(config).map_err(|e| anyhow::anyhow!("invalid guard config: {e:?}"))?;
    let mut oracle = SlotTwapOracle::new(config.min_observations);

    let mut report = AnalysisReport {
        cu_overhead_per_swap: cu::estimate_check_cu(config.min_observations),
        ..Default::default()
    };
    let mut score = LabelledScore::default();
    let mut any_labelled = false;

    let mut last_slot: Option<u64> = None;

    for ev in events {
        let price = to_fixed(ev.price)
            .with_context(|| format!("converting price for tx {}", ev.signature))?;

        // Defend against unsorted input: the oracle enforces monotonic slots,
        // but we want a clear error rather than a silent skip.
        if let Some(prev) = last_slot {
            anyhow::ensure!(
                ev.slot >= prev,
                "events must be sorted by slot ascending: {} after {}",
                ev.slot,
                prev
            );
        }

        let decision = guard
            .check_swap(&oracle, price, ev.slot)
            .map_err(|e| anyhow::anyhow!("guard error on tx {}: {e:?}", ev.signature))?;

        match decision {
            GuardDecision::NotReady => {
                report.warmup_skipped += 1;
            }
            GuardDecision::Allow { .. } => {
                report.evaluated += 1;
                report.allowed += 1;
                if let Some(is_attack) = ev.is_attack {
                    any_labelled = true;
                    if is_attack {
                        score.false_negatives += 1;
                    } else {
                        score.true_negatives += 1;
                    }
                }
            }
            GuardDecision::Reject { .. } => {
                report.evaluated += 1;
                report.rejected += 1;
                if let Some(is_attack) = ev.is_attack {
                    any_labelled = true;
                    if is_attack {
                        score.true_positives += 1;
                    } else {
                        score.false_positives += 1;
                    }
                }
            }
        }

        // Update history only when we actually have a slot advance. Multiple
        // swaps can share a slot; we record the last price seen in that slot by
        // only observing when the slot strictly increases.
        let should_observe = match oracle.latest() {
            Some(obs) => ev.slot > obs.slot,
            None => true,
        };
        if should_observe {
            // Ignore monotonic errors here: they cannot occur given the guard
            // above, but if they did we prefer to keep replaying.
            let _ = oracle.observe(ev.slot, price);
        }

        last_slot = Some(ev.slot);
    }

    if any_labelled {
        report.labelled = Some(score);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(slot: u64, price: f64, attack: Option<bool>) -> SwapEvent {
        SwapEvent {
            slot,
            signature: format!("sig{slot}"),
            price,
            is_attack: attack,
        }
    }

    fn cfg() -> GuardConfig {
        GuardConfig {
            tolerance_bps: 200,
            min_observations: 3,
        }
    }

    #[test]
    fn warmup_swaps_are_skipped() {
        let events = vec![ev(1, 100.0, None), ev(2, 100.0, None)];
        let report = replay(&events, cfg()).unwrap();
        // min_observations is 3, so neither swap can be judged.
        assert_eq!(report.warmup_skipped, 2);
        assert_eq!(report.evaluated, 0);
    }

    #[test]
    fn stable_market_allows_everything() {
        let events = vec![
            ev(1, 100.0, None),
            ev(2, 100.0, None),
            ev(3, 100.0, None),
            ev(4, 100.5, None),
            ev(5, 99.7, None),
        ];
        let report = replay(&events, cfg()).unwrap();
        assert_eq!(report.rejected, 0);
        assert!(report.allowed >= 1);
    }

    #[test]
    fn cross_slot_spike_is_flagged() {
        // Stable 100 for several slots, then a sharp jump to 130 over one slot.
        let events = vec![
            ev(1, 100.0, Some(false)),
            ev(2, 100.0, Some(false)),
            ev(10, 100.0, Some(false)),
            ev(20, 100.0, Some(false)),
            ev(40, 130.0, Some(true)), // manipulated swap
        ];
        let report = replay(&events, cfg()).unwrap();
        assert_eq!(report.rejected, 1);
        let score = report.labelled.unwrap();
        assert_eq!(score.true_positives, 1);
        assert_eq!(score.detection_rate(), 1.0);
    }

    #[test]
    fn rejects_unsorted_input() {
        let events = vec![ev(5, 100.0, None), ev(3, 100.0, None)];
        let err = replay(&events, cfg()).unwrap_err();
        assert!(err.to_string().contains("sorted"));
    }
}

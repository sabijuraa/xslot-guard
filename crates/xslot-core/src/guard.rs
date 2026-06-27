//! The cross-slot deviation guard.
//!
//! This is the decision layer that protocols actually call. Given a configured
//! tolerance and a populated [`SlotTwapOracle`], it answers one question for an
//! incoming swap: *is this execution price consistent with the recent
//! slot-weighted history, or has someone manipulated the pool across slots to
//! sandwich this trade?*

use crate::error::GuardError;
use crate::oracle::SlotTwapOracle;
use crate::price::FixedPrice;

/// Configuration for the guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardConfig {
    /// Maximum allowed deviation of a swap price from the TWAP, in basis
    /// points. A swap deviating beyond this is rejected.
    ///
    /// Must be in `(0, 10_000)`. Typical production values: 50–300 bps.
    pub tolerance_bps: u64,

    /// Minimum observations the oracle must hold before the guard enforces.
    /// Below this the guard reports [`GuardDecision::NotReady`] and the caller
    /// decides whether to fail open or closed.
    pub min_observations: usize,
}

impl GuardConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), GuardError> {
        if self.tolerance_bps == 0 || self.tolerance_bps >= crate::price::BPS_DENOM {
            return Err(GuardError::InvalidTolerance);
        }
        Ok(())
    }
}

/// The guard's verdict for a candidate swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardDecision {
    /// Price is within tolerance. The recorded deviation is included so callers
    /// can log or meter it.
    Allow {
        /// The measured deviation of the swap price from the TWAP, in bps.
        deviation_bps: u64,
    },
    /// Price deviates beyond tolerance — likely cross-slot manipulation.
    Reject {
        /// The measured deviation of the swap price from the TWAP, in bps.
        deviation_bps: u64,
        /// The configured tolerance that was exceeded, in bps.
        tolerance_bps: u64,
    },
    /// The oracle is still warming up; not enough history to judge.
    NotReady,
}

impl GuardDecision {
    /// Whether this decision permits the swap to proceed.
    #[inline]
    pub fn is_allowed(self) -> bool {
        matches!(self, GuardDecision::Allow { .. })
    }

    /// Whether this decision blocks the swap.
    #[inline]
    pub fn is_rejected(self) -> bool {
        matches!(self, GuardDecision::Reject { .. })
    }
}

/// Stateless guard evaluator. Holds configuration; reads state from the oracle
/// passed in. Keeping it stateless makes it trivial to embed in an Anchor
/// instruction where the oracle lives in an account.
#[derive(Clone, Copy, Debug)]
pub struct CrossSlotGuard {
    config: GuardConfig,
}

impl CrossSlotGuard {
    /// Build a guard from validated configuration.
    pub fn new(config: GuardConfig) -> Result<Self, GuardError> {
        config.validate()?;
        Ok(Self { config })
    }

    /// The active configuration.
    #[inline]
    pub fn config(&self) -> GuardConfig {
        self.config
    }

    /// Evaluate a candidate swap price against the oracle's TWAP as of
    /// `current_slot`.
    ///
    /// This does not mutate the oracle. The typical on-chain flow is:
    /// 1. read the oracle account,
    /// 2. call [`check_swap`](Self::check_swap) with the intended execution
    ///    price,
    /// 3. if allowed, perform the swap and then [`observe`] the realized price.
    ///
    /// Returning a structured [`GuardDecision`] instead of a bare `Result` lets
    /// the caller distinguish "not ready" (a policy choice) from "rejected"
    /// (a hard manipulation signal).
    pub fn check_swap(
        &self,
        oracle: &SlotTwapOracle,
        swap_price: FixedPrice,
        current_slot: u64,
    ) -> Result<GuardDecision, GuardError> {
        if !oracle.is_ready() {
            return Ok(GuardDecision::NotReady);
        }

        let twap = oracle.twap(current_slot)?;
        let deviation_bps = swap_price.deviation_bps(twap)?;

        if deviation_bps > self.config.tolerance_bps {
            Ok(GuardDecision::Reject {
                deviation_bps,
                tolerance_bps: self.config.tolerance_bps,
            })
        } else {
            Ok(GuardDecision::Allow { deviation_bps })
        }
    }

    /// Convenience that collapses the decision into a `Result`, mapping a
    /// rejection onto [`GuardError::DeviationExceeded`]. Useful for on-chain
    /// code that wants a fail-closed `require!`-style call.
    ///
    /// `NotReady` maps to `Ok(())` (fail open during warmup); change this to
    /// fail closed by returning `InsufficientHistory` if your protocol prefers.
    pub fn require_within_tolerance(
        &self,
        oracle: &SlotTwapOracle,
        swap_price: FixedPrice,
        current_slot: u64,
    ) -> Result<(), GuardError> {
        match self.check_swap(oracle, swap_price, current_slot)? {
            GuardDecision::Allow { .. } | GuardDecision::NotReady => Ok(()),
            GuardDecision::Reject { .. } => Err(GuardError::DeviationExceeded),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(v: u64) -> FixedPrice {
        FixedPrice::from_scaled(v, 0).unwrap()
    }

    fn ready_oracle() -> SlotTwapOracle {
        let mut o = SlotTwapOracle::new(2);
        // Stable price 100 held across many slots.
        o.observe(0, px(100)).unwrap();
        o.observe(10, px(100)).unwrap();
        o.observe(20, px(100)).unwrap();
        o
    }

    #[test]
    fn config_rejects_bad_tolerance() {
        assert_eq!(
            GuardConfig { tolerance_bps: 0, min_observations: 2 }.validate(),
            Err(GuardError::InvalidTolerance)
        );
        assert_eq!(
            GuardConfig { tolerance_bps: 10_000, min_observations: 2 }.validate(),
            Err(GuardError::InvalidTolerance)
        );
        assert!(GuardConfig { tolerance_bps: 100, min_observations: 2 }
            .validate()
            .is_ok());
    }

    #[test]
    fn allows_price_within_tolerance() {
        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: 200, // 2%
            min_observations: 2,
        })
        .unwrap();
        let oracle = ready_oracle();
        // Swap at 101 == 100 bps deviation from TWAP 100, within 200.
        let decision = guard.check_swap(&oracle, px(101), 21).unwrap();
        assert!(decision.is_allowed());
        match decision {
            GuardDecision::Allow { deviation_bps } => assert_eq!(deviation_bps, 100),
            _ => panic!("expected Allow"),
        }
    }

    #[test]
    fn rejects_price_beyond_tolerance() {
        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: 200, // 2%
            min_observations: 2,
        })
        .unwrap();
        let oracle = ready_oracle();
        // Swap at 110 == 1000 bps deviation, beyond 200.
        let decision = guard.check_swap(&oracle, px(110), 21).unwrap();
        assert!(decision.is_rejected());
    }

    #[test]
    fn not_ready_when_oracle_cold() {
        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: 200,
            min_observations: 5,
        })
        .unwrap();
        let mut oracle = SlotTwapOracle::new(5);
        oracle.observe(0, px(100)).unwrap();
        let decision = guard.check_swap(&oracle, px(100), 1).unwrap();
        assert_eq!(decision, GuardDecision::NotReady);
    }

    #[test]
    fn cross_slot_sandwich_is_rejected() {
        // Realistic scenario: honest price 100 for 20 slots. Attacker pushes
        // price to 130 in slot 20 (front-run in a prior slot). Victim swap
        // lands slot 21 at the manipulated 130.
        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: 150, // 1.5% tolerance
            min_observations: 2,
        })
        .unwrap();
        let mut oracle = SlotTwapOracle::new(2);
        oracle.observe(0, px(100)).unwrap();
        oracle.observe(20, px(130)).unwrap();
        // TWAP as of slot 21 is dominated by the 20-slot dwell at 100, so the
        // 130 swap deviates far beyond 150 bps and is rejected.
        let decision = guard.check_swap(&oracle, px(130), 21).unwrap();
        assert!(decision.is_rejected());
    }

    #[test]
    fn require_helper_maps_to_error() {
        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: 100,
            min_observations: 2,
        })
        .unwrap();
        let oracle = ready_oracle();
        assert_eq!(
            guard.require_within_tolerance(&oracle, px(120), 21),
            Err(GuardError::DeviationExceeded)
        );
        assert!(guard
            .require_within_tolerance(&oracle, px(100), 21)
            .is_ok());
    }
}

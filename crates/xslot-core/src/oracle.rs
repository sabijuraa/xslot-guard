//! Slot-indexed time-weighted average price (TWAP) oracle.
//!
//! # Why slot-weighted, not count-weighted
//!
//! A naive moving average weights every observation equally. That is exactly
//! what a cross-slot sandwich exploits: the attacker pushes one manipulated
//! observation into the buffer and it counts as much as an honest one.
//!
//! We instead weight each observation by the number of slots it was the
//! prevailing price — its *dwell time*. A price that existed for one slot
//! before the attacker moved it contributes one slot of weight; the honest
//! price that held for the preceding twenty slots contributes twenty. A
//! single-slot manipulation therefore barely moves the TWAP, and the
//! subsequent swap at the manipulated price deviates sharply from it.
//!
//! This is the on-chain analogue of Uniswap v2/v3's cumulative-price oracle,
//! adapted to Solana's slot clock and sized for a bounded ring buffer that
//! fits in a program account.

use crate::error::GuardError;
use crate::price::FixedPrice;
#[cfg(feature = "std")]
use crate::price::SCALE;

/// Maximum number of observations retained in the ring buffer.
///
/// Sized so the whole oracle account stays small: 32 observations * 24 bytes
/// == 768 bytes of samples plus a small header. At ~2.5 slots/sec on Solana,
/// 32 slots of history is ~13 seconds — long enough to span the 1–3 slot
/// window the ACM IMC 2025 paper attributes to 93% of sandwich attacks, with
/// margin.
pub const MAX_OBSERVATIONS: usize = 32;

/// A single price sample anchored to the slot at which it was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Slot at which this price became the prevailing price.
    pub slot: u64,
    /// The recorded price in fixed-point form.
    pub price: FixedPrice,
}

/// A bounded, slot-weighted TWAP oracle backed by a ring buffer.
///
/// The oracle is `Copy`-free but fixed-size, so it serializes deterministically
/// into a Solana account. All math is checked; there are no floats and no
/// allocation.
#[derive(Clone, Debug)]
pub struct SlotTwapOracle {
    /// Ring buffer of observations. Only the first `len` entries are valid.
    observations: [Option<Observation>; MAX_OBSERVATIONS],
    /// Index where the next observation will be written.
    head: usize,
    /// Number of valid observations currently stored (<= MAX_OBSERVATIONS).
    len: usize,
    /// Minimum number of observations before the TWAP is considered usable.
    min_observations: usize,
}

impl SlotTwapOracle {
    /// Create an empty oracle requiring `min_observations` before it will
    /// produce a TWAP.
    ///
    /// `min_observations` is clamped to `[2, MAX_OBSERVATIONS]`: a TWAP needs at
    /// least two observations to define a time window.
    pub fn new(min_observations: usize) -> Self {
        let clamped = min_observations.clamp(2, MAX_OBSERVATIONS);
        Self {
            observations: [None; MAX_OBSERVATIONS],
            head: 0,
            len: 0,
            min_observations: clamped,
        }
    }

    /// Number of observations currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the oracle holds no observations.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the oracle has enough history to produce a trustworthy TWAP.
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.len >= self.min_observations
    }

    /// The most recently recorded observation, if any.
    pub fn latest(&self) -> Option<Observation> {
        if self.len == 0 {
            return None;
        }
        // head points one past the newest; step back with wraparound.
        let idx = (self.head + MAX_OBSERVATIONS - 1) % MAX_OBSERVATIONS;
        self.observations[idx]
    }

    /// Record a new observation.
    ///
    /// Enforces strictly increasing slots: an observation at a slot equal to or
    /// below the latest is rejected with [`GuardError::NonMonotonicSlot`]. This
    /// closes a replay vector where an attacker resubmits a stale favorable
    /// price.
    pub fn observe(&mut self, slot: u64, price: FixedPrice) -> Result<(), GuardError> {
        if let Some(last) = self.latest() {
            if slot <= last.slot {
                return Err(GuardError::NonMonotonicSlot);
            }
        }

        self.observations[self.head] = Some(Observation { slot, price });
        self.head = (self.head + 1) % MAX_OBSERVATIONS;
        if self.len < MAX_OBSERVATIONS {
            self.len += 1;
        }
        Ok(())
    }

    /// Iterate stored observations from oldest to newest.
    fn iter_chrono(&self) -> impl Iterator<Item = Observation> + '_ {
        let start = if self.len < MAX_OBSERVATIONS {
            0
        } else {
            self.head
        };
        (0..self.len).filter_map(move |i| {
            let idx = (start + i) % MAX_OBSERVATIONS;
            self.observations[idx]
        })
    }

    /// Public chronological accessor over stored observations (oldest first).
    ///
    /// Exposed so an on-chain account wrapper can serialize the oracle into a
    /// canonical array form without duplicating the ring-buffer bookkeeping.
    pub fn observations_chrono(&self) -> impl Iterator<Item = Observation> + '_ {
        self.iter_chrono()
    }

    /// Compute the slot-weighted TWAP over all stored observations, evaluated
    /// as of `current_slot`.
    ///
    /// Each observation `i` is weighted by its dwell time: the number of slots
    /// until the next observation (or until `current_slot` for the newest).
    /// The result is `sum(price_i * dwell_i) / sum(dwell_i)`.
    ///
    /// # Errors
    /// - [`GuardError::InsufficientHistory`] if the oracle is not ready.
    /// - [`GuardError::ZeroWindow`] if total dwell time is zero.
    /// - [`GuardError::Overflow`] on arithmetic overflow.
    pub fn twap(&self, current_slot: u64) -> Result<FixedPrice, GuardError> {
        if !self.is_ready() {
            return Err(GuardError::InsufficientHistory);
        }

        let samples: heapless_vec::Vec = {
            let mut v = heapless_vec::Vec::new();
            for obs in self.iter_chrono() {
                v.push(obs);
            }
            v
        };

        let mut weighted_sum: u128 = 0;
        let mut total_weight: u128 = 0;

        for i in 0..samples.len {
            let obs = samples.data[i];
            let end_slot = if i + 1 < samples.len {
                samples.data[i + 1].slot
            } else {
                current_slot
            };

            // The newest observation gets at least one slot of weight via max(1)
            // so the latest price is never dropped from the average.
            let dwell = end_slot.saturating_sub(obs.slot).max(1) as u128;

            let contribution = obs
                .price
                .raw()
                .checked_mul(dwell)
                .ok_or(GuardError::Overflow)?;
            weighted_sum = weighted_sum
                .checked_add(contribution)
                .ok_or(GuardError::Overflow)?;
            total_weight = total_weight
                .checked_add(dwell)
                .ok_or(GuardError::Overflow)?;
        }

        if total_weight == 0 {
            return Err(GuardError::ZeroWindow);
        }

        let twap_raw = weighted_sum
            .checked_div(total_weight)
            .ok_or(GuardError::Overflow)?;
        FixedPrice::from_raw(twap_raw)
    }

    /// Convenience: produce a human-readable TWAP as an `f64`. Host/off-chain
    /// only — never call from on-chain code.
    #[cfg(feature = "std")]
    pub fn twap_f64(&self, current_slot: u64) -> Result<f64, GuardError> {
        let raw = self.twap(current_slot)?.raw();
        Ok(raw as f64 / SCALE as f64)
    }
}

/// A tiny fixed-capacity vector so the TWAP computation needs no allocation and
/// stays `no_std`. Capacity equals [`MAX_OBSERVATIONS`].
mod heapless_vec {
    use super::{Observation, MAX_OBSERVATIONS};

    pub struct Vec {
        pub data: [Observation; MAX_OBSERVATIONS],
        pub len: usize,
    }

    impl Vec {
        pub fn new() -> Self {
            // Observation is Copy; seed with a zeroed placeholder that is always
            // overwritten before `len` is advanced.
            let placeholder = Observation {
                slot: 0,
                // SAFETY of value: 1 is a valid non-zero raw price; it is never
                // read because indices >= len are never accessed.
                price: super::FixedPrice::from_raw(1).expect("1 is non-zero"),
            };
            Self {
                data: [placeholder; MAX_OBSERVATIONS],
                len: 0,
            }
        }

        pub fn push(&mut self, obs: Observation) {
            if self.len < MAX_OBSERVATIONS {
                self.data[self.len] = obs;
                self.len += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(v: u64) -> FixedPrice {
        FixedPrice::from_scaled(v, 0).unwrap()
    }

    #[test]
    fn not_ready_until_min_observations() {
        let mut o = SlotTwapOracle::new(3);
        assert!(!o.is_ready());
        o.observe(1, px(100)).unwrap();
        o.observe(2, px(100)).unwrap();
        assert!(!o.is_ready());
        o.observe(3, px(100)).unwrap();
        assert!(o.is_ready());
    }

    #[test]
    fn rejects_non_monotonic_slot() {
        let mut o = SlotTwapOracle::new(2);
        o.observe(10, px(100)).unwrap();
        assert_eq!(o.observe(10, px(100)), Err(GuardError::NonMonotonicSlot));
        assert_eq!(o.observe(9, px(100)), Err(GuardError::NonMonotonicSlot));
    }

    #[test]
    fn constant_price_twap_is_constant() {
        let mut o = SlotTwapOracle::new(2);
        o.observe(1, px(100)).unwrap();
        o.observe(2, px(100)).unwrap();
        o.observe(3, px(100)).unwrap();
        let twap = o.twap(4).unwrap();
        assert_eq!(twap, px(100));
    }

    #[test]
    fn single_slot_spike_barely_moves_twap() {
        // Honest price 100 held for 20 slots, then a 1-slot spike to 200.
        let mut o = SlotTwapOracle::new(2);
        o.observe(0, px(100)).unwrap();
        o.observe(20, px(200)).unwrap();
        // Evaluate one slot after the spike.
        let twap = o.twap(21).unwrap();
        // Weights: 100 dwelled slots 0->20 == 20; 200 dwelled 20->21 == 1.
        // TWAP = (100*20 + 200*1) / 21 = 2200/21 = 104.76...
        let expected = (100 * 20 + 200 * 1) * SCALE / 21;
        assert_eq!(twap.raw(), expected);
        // The manipulated 200 only dragged the average to ~104.8, so a swap at
        // 200 will show a large deviation from this TWAP.
        let dev = px(200).deviation_bps(twap).unwrap();
        assert!(dev > 9000, "expected large deviation, got {dev} bps");
    }

    #[test]
    fn ring_buffer_wraps_and_keeps_newest() {
        let mut o = SlotTwapOracle::new(2);
        // Push MAX_OBSERVATIONS + 5 observations.
        for s in 0..(MAX_OBSERVATIONS as u64 + 5) {
            o.observe(s, px(100 + s)).unwrap();
        }
        assert_eq!(o.len(), MAX_OBSERVATIONS);
        let latest = o.latest().unwrap();
        assert_eq!(latest.slot, MAX_OBSERVATIONS as u64 + 4);
    }

    #[test]
    fn insufficient_history_errors() {
        let o = SlotTwapOracle::new(4);
        assert_eq!(o.twap(1), Err(GuardError::InsufficientHistory));
    }
}

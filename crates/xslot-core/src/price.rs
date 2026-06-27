//! Fixed-point price representation.
//!
//! On-chain code cannot use floating point: `f64` is non-deterministic across
//! BPF and host, and Solana's runtime disallows float syscalls in the hot path.
//! We represent prices as a `u128` numerator over a fixed `SCALE` denominator.
//!
//! A price `p` is stored as `round(p * SCALE)`. With `SCALE = 1e9` we keep nine
//! decimal places of precision, which comfortably exceeds the precision of any
//! Solana CLMM `sqrtPriceX64`-derived price after conversion.

use crate::error::GuardError;

/// Fixed-point scaling factor (1e9 -> nine decimal places).
pub const SCALE: u128 = 1_000_000_000;

/// Basis-points denominator. 10_000 bps == 100%.
pub const BPS_DENOM: u64 = 10_000;

/// A non-negative price in fixed-point form (`raw == price * SCALE`).
///
/// Invariant: a valid `FixedPrice` is always strictly positive. The constructor
/// rejects zero so downstream weighting math never divides by zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FixedPrice {
    raw: u128,
}

impl FixedPrice {
    /// Construct from an already-scaled raw value, rejecting zero.
    #[inline]
    pub const fn from_raw(raw: u128) -> Result<Self, GuardError> {
        if raw == 0 {
            return Err(GuardError::ZeroPrice);
        }
        Ok(Self { raw })
    }

    /// Construct from an integer price and a number of decimals.
    ///
    /// `from_scaled(1234, 2)` represents `12.34`.
    pub fn from_scaled(value: u64, decimals: u32) -> Result<Self, GuardError> {
        let pow = 10u128.checked_pow(decimals).ok_or(GuardError::Overflow)?;
        let scaled = (value as u128)
            .checked_mul(SCALE)
            .ok_or(GuardError::Overflow)?
            .checked_div(pow)
            .ok_or(GuardError::Overflow)?;
        Self::from_raw(scaled)
    }

    /// The underlying scaled value.
    #[inline]
    pub const fn raw(self) -> u128 {
        self.raw
    }

    /// Absolute deviation between `self` and `other`, expressed in basis points
    /// relative to `reference`.
    ///
    /// `bps = |self - other| * 10_000 / reference`
    ///
    /// We always measure relative to the TWAP (`reference`), not to the larger
    /// of the two, because the TWAP is the trusted baseline and an attacker
    /// controls the candidate price.
    pub fn deviation_bps(self, reference: FixedPrice) -> Result<u64, GuardError> {
        let diff = if self.raw >= reference.raw {
            self.raw - reference.raw
        } else {
            reference.raw - self.raw
        };

        let bps = diff
            .checked_mul(BPS_DENOM as u128)
            .ok_or(GuardError::Overflow)?
            .checked_div(reference.raw)
            .ok_or(GuardError::Overflow)?;

        // bps can legitimately exceed u64 only for absurd (>1.8e19 bps) moves;
        // clamp defensively rather than overflow.
        Ok(u64::try_from(bps).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero() {
        assert_eq!(FixedPrice::from_raw(0), Err(GuardError::ZeroPrice));
        assert_eq!(FixedPrice::from_scaled(0, 2), Err(GuardError::ZeroPrice));
    }

    #[test]
    fn from_scaled_basic() {
        // 12.34 with 2 decimals -> 12.34 * 1e9 = 12_340_000_000
        let p = FixedPrice::from_scaled(1234, 2).unwrap();
        assert_eq!(p.raw(), 12_340_000_000);
    }

    #[test]
    fn deviation_symmetric_magnitude() {
        let twap = FixedPrice::from_scaled(100, 0).unwrap();
        let up = FixedPrice::from_scaled(101, 0).unwrap();
        let down = FixedPrice::from_scaled(99, 0).unwrap();
        // Both are 1% == 100 bps relative to the 100 TWAP.
        assert_eq!(up.deviation_bps(twap).unwrap(), 100);
        assert_eq!(down.deviation_bps(twap).unwrap(), 100);
    }

    #[test]
    fn deviation_zero_when_equal() {
        let a = FixedPrice::from_scaled(500, 1).unwrap();
        assert_eq!(a.deviation_bps(a).unwrap(), 0);
    }

    #[test]
    fn deviation_large_move() {
        let twap = FixedPrice::from_scaled(100, 0).unwrap();
        let spike = FixedPrice::from_scaled(150, 0).unwrap();
        // 50% == 5000 bps
        assert_eq!(spike.deviation_bps(twap).unwrap(), 5000);
    }
}

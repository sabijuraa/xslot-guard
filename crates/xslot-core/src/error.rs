//! Error types for the cross-slot guard core.
//!
//! These are deliberately `Copy` and carry a stable `u32` code so the on-chain
//! program can map them directly onto Anchor error codes without allocation.

/// Errors produced by the slot-indexed TWAP oracle and the deviation guard.
///
/// The discriminants are part of the public ABI: the on-chain program maps
/// each variant onto a fixed Anchor error code. Do not reorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(thiserror::Error))]
pub enum GuardError {
    /// A price of zero was supplied. Zero prices are never valid and usually
    /// indicate an uninitialized account or a corrupted oracle read.
    #[cfg_attr(feature = "std", error("price must be non-zero"))]
    ZeroPrice = 0,

    /// The observation's slot is not strictly greater than the most recent
    /// stored observation. Equal or decreasing slots break the time-weighting
    /// invariant and can be used to replay stale prices.
    #[cfg_attr(feature = "std", error("slot must be strictly increasing"))]
    NonMonotonicSlot = 1,

    /// The oracle has not yet accumulated enough observations to produce a
    /// trustworthy TWAP. Callers must treat the guard as "not ready" rather
    /// than fail open.
    #[cfg_attr(feature = "std", error("oracle is still warming up"))]
    InsufficientHistory = 2,

    /// The candidate swap price deviates from the slot-weighted TWAP by more
    /// than the configured tolerance. This is the core rejection: it is the
    /// signal of a cross-slot sandwich manipulating the pool mid-price.
    #[cfg_attr(feature = "std", error("price deviation exceeds tolerance"))]
    DeviationExceeded = 3,

    /// An arithmetic operation overflowed. On-chain this must always abort the
    /// transaction rather than wrap, since wrapping a price or accumulator
    /// silently corrupts the oracle.
    #[cfg_attr(feature = "std", error("arithmetic overflow"))]
    Overflow = 4,

    /// The configured tolerance is outside the sane range (0, 10_000) bps.
    #[cfg_attr(feature = "std", error("tolerance out of range"))]
    InvalidTolerance = 5,

    /// The time window between the oldest and newest observation is zero, so a
    /// time-weighted average is undefined.
    #[cfg_attr(feature = "std", error("zero time window"))]
    ZeroWindow = 6,
}

impl GuardError {
    /// Stable numeric code for cross-boundary mapping (e.g. Anchor error codes).
    #[inline]
    pub const fn code(self) -> u32 {
        self as u32
    }
}

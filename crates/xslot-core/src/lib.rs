//! # xslot-core
//!
//! Core algorithm for detecting cross-slot sandwich MEV on Solana.
//!
//! Background: the ACM IMC 2025 study of Solana MEV found that **93% of
//! sandwich attacks span more than one validator slot** — the attacker
//! front-runs the victim in slot *N* and back-runs in slot *N+1* or *N+2*.
//! Defenses that only inspect a single transaction or a single slot miss the
//! overwhelming majority of real attacks.
//!
//! This crate provides the building blocks for a defense that works across
//! slots:
//!
//! - [`price::FixedPrice`] — float-free fixed-point price arithmetic safe for
//!   the Solana BPF runtime.
//! - [`oracle::SlotTwapOracle`] — a bounded, slot-weighted TWAP oracle. By
//!   weighting each price by how many slots it actually prevailed, a one-slot
//!   manipulation contributes almost nothing to the average.
//! - [`guard::CrossSlotGuard`] — the decision layer: reject any swap whose
//!   price deviates from the slot-weighted TWAP by more than a configured
//!   tolerance.
//!
//! The crate is `no_std` by default-disable: enable the `std` feature for
//! off-chain tooling (it pulls in `thiserror` and `f64` helpers). On-chain the
//! Anchor program depends on it with `default-features = false`.
//!
//! ## Example
//!
//! ```
//! use xslot_core::{CrossSlotGuard, GuardConfig, SlotTwapOracle, FixedPrice};
//!
//! let mut oracle = SlotTwapOracle::new(2);
//! oracle.observe(0, FixedPrice::from_scaled(100, 0).unwrap()).unwrap();
//! oracle.observe(20, FixedPrice::from_scaled(100, 0).unwrap()).unwrap();
//!
//! let guard = CrossSlotGuard::new(GuardConfig {
//!     tolerance_bps: 150,
//!     min_observations: 2,
//! }).unwrap();
//!
//! // A swap at 130 against a TWAP of ~100 is rejected as cross-slot manipulation.
//! let decision = guard
//!     .check_swap(&oracle, FixedPrice::from_scaled(130, 0).unwrap(), 21)
//!     .unwrap();
//! assert!(decision.is_rejected());
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod guard;
pub mod oracle;
pub mod price;

pub use error::GuardError;
pub use guard::{CrossSlotGuard, GuardConfig, GuardDecision};
pub use oracle::{Observation, SlotTwapOracle, MAX_OBSERVATIONS};
pub use price::{FixedPrice, BPS_DENOM, SCALE};

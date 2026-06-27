//! # xslot-guard (on-chain program)
//!
//! An Anchor program that protects Solana DeFi swaps from **cross-slot sandwich
//! MEV** — the attack class the ACM IMC 2025 study found accounts for 93% of
//! sandwich attacks on Solana, where the front-run and back-run land in
//! different validator slots.
//!
//! ## How a protocol integrates it
//!
//! 1. Once, call [`initialize_guard`] to create a [`GuardOracle`] PDA for the
//!    pool, choosing a deviation `tolerance_bps` and `min_observations`.
//! 2. On every price update (e.g. after each swap, or on an oracle tick) call
//!    [`record_observation`] with the current pool price and slot.
//! 3. Before executing a swap, CPI into [`check_swap`] (or call it inline). It
//!    reads the slot-weighted TWAP from the oracle and **fails the transaction**
//!    if the swap price deviates beyond tolerance — which is exactly the
//!    signature of a cross-slot manipulation.
//!
//! All numeric logic is delegated to the audited, unit-tested `xslot-core`
//! crate, compiled with `default-features = false` so it is `no_std` and
//! float-free on BPF. The program is a thin, security-focused wrapper:
//! account validation, PDA constraints, and error mapping.

use anchor_lang::prelude::*;
use xslot_core::{CrossSlotGuard, FixedPrice, GuardConfig, GuardDecision, SlotTwapOracle};

declare_id!("xsLoTGuard1111111111111111111111111111111111");

#[program]
pub mod xslot_guard {
    use super::*;

    /// Create and configure a guard oracle PDA for a pool.
    ///
    /// The PDA is derived from `[b"guard", pool.key()]`, so each pool has
    /// exactly one guard and integrators can find it deterministically.
    pub fn initialize_guard(
        ctx: Context<InitializeGuard>,
        tolerance_bps: u64,
        min_observations: u8,
    ) -> Result<()> {
        // Validate config through the core type so on-chain and off-chain agree
        // on what "valid" means.
        let config = GuardConfig {
            tolerance_bps,
            min_observations: min_observations as usize,
        };
        config
            .validate()
            .map_err(|e| map_core_err(e))?;

        let oracle = &mut ctx.accounts.guard_oracle;
        oracle.authority = ctx.accounts.authority.key();
        oracle.pool = ctx.accounts.pool.key();
        oracle.tolerance_bps = tolerance_bps;
        oracle.min_observations = min_observations;
        oracle.head = 0;
        oracle.len = 0;
        oracle.slots = [0; xslot_core::MAX_OBSERVATIONS];
        oracle.prices = [0; xslot_core::MAX_OBSERVATIONS];
        oracle.bump = ctx.bumps.guard_oracle;

        emit!(GuardInitialized {
            pool: oracle.pool,
            tolerance_bps,
            min_observations,
        });
        Ok(())
    }

    /// Record a new price observation for the pool.
    ///
    /// `price_raw` is the price in `xslot_core` fixed-point form
    /// (`price * 1e9`). The caller is responsible for deriving it from the
    /// pool's post-swap `sqrtPrice`; doing the conversion off the hot path keeps
    /// this instruction cheap. Slots must strictly increase.
    pub fn record_observation(
        ctx: Context<RecordObservation>,
        slot: u64,
        price_raw: u128,
    ) -> Result<()> {
        let oracle_acc = &mut ctx.accounts.guard_oracle;

        // Rebuild the in-memory oracle from account state, apply the new
        // observation through the core (which enforces monotonic slots and
        // non-zero price), then persist back.
        let mut oracle = oracle_acc.to_core();
        let price = FixedPrice::from_raw(price_raw).map_err(map_core_err)?;
        oracle.observe(slot, price).map_err(map_core_err)?;
        oracle_acc.store_core(&oracle);

        Ok(())
    }

    /// Check a candidate swap price against the slot-weighted TWAP.
    ///
    /// Fails with [`GuardErrorCode::DeviationExceeded`] if the price deviates
    /// beyond the configured tolerance. Returns `Ok(())` when the swap is safe
    /// or when the oracle is still warming up (fail-open during warmup; a
    /// protocol wanting fail-closed can check readiness separately).
    ///
    /// Designed to be called via CPI from a host DEX program immediately before
    /// it settles the swap.
    pub fn check_swap(
        ctx: Context<CheckSwap>,
        swap_price_raw: u128,
        current_slot: u64,
    ) -> Result<()> {
        let oracle_acc = &ctx.accounts.guard_oracle;
        let oracle = oracle_acc.to_core();

        let guard = CrossSlotGuard::new(GuardConfig {
            tolerance_bps: oracle_acc.tolerance_bps,
            min_observations: oracle_acc.min_observations as usize,
        })
        .map_err(map_core_err)?;

        let swap_price = FixedPrice::from_raw(swap_price_raw).map_err(map_core_err)?;
        let decision = guard
            .check_swap(&oracle, swap_price, current_slot)
            .map_err(map_core_err)?;

        match decision {
            GuardDecision::Allow { deviation_bps } => {
                emit!(SwapChecked {
                    pool: oracle_acc.pool,
                    deviation_bps,
                    allowed: true,
                });
                Ok(())
            }
            GuardDecision::NotReady => {
                // Warming up: allow, but surface it so integrators can choose to
                // fail closed if they prefer.
                emit!(SwapChecked {
                    pool: oracle_acc.pool,
                    deviation_bps: 0,
                    allowed: true,
                });
                Ok(())
            }
            GuardDecision::Reject {
                deviation_bps,
                tolerance_bps,
            } => {
                emit!(SwapChecked {
                    pool: oracle_acc.pool,
                    deviation_bps,
                    allowed: false,
                });
                msg!(
                    "xslot-guard: rejecting swap, deviation {} bps exceeds tolerance {} bps",
                    deviation_bps,
                    tolerance_bps
                );
                Err(GuardErrorCode::DeviationExceeded.into())
            }
        }
    }
}

/// The on-chain oracle account.
///
/// We store the ring buffer as parallel `slots`/`prices` arrays of fixed size
/// so the account has a constant, known size and (de)serializes deterministically
/// with Anchor's zero-copy-friendly layout. The in-memory [`SlotTwapOracle`] is
/// reconstructed on each instruction via [`GuardOracle::to_core`].
#[account]
pub struct GuardOracle {
    /// Authority allowed to administer the guard.
    pub authority: Pubkey,
    /// The pool this guard protects.
    pub pool: Pubkey,
    /// Deviation tolerance in basis points.
    pub tolerance_bps: u64,
    /// Minimum observations before enforcement.
    pub min_observations: u8,
    /// PDA bump.
    pub bump: u8,
    /// Ring-buffer write head.
    pub head: u8,
    /// Number of valid observations.
    pub len: u8,
    /// Observation slots (parallel with `prices`).
    pub slots: [u64; xslot_core::MAX_OBSERVATIONS],
    /// Observation prices in fixed-point raw form (parallel with `slots`).
    pub prices: [u128; xslot_core::MAX_OBSERVATIONS],
}

impl GuardOracle {
    /// Account size for rent calculation.
    /// 32 + 32 + 8 + 1 + 1 + 1 + 1 + (8*32) + (16*32) = 845 bytes (+8 discriminator).
    pub const LEN: usize = 8 + 32 + 32 + 8 + 1 + 1 + 1 + 1 + (8 * 32) + (16 * 32);

    /// Reconstruct the in-memory core oracle from stored arrays.
    ///
    /// We replay the stored observations in chronological order into a fresh
    /// [`SlotTwapOracle`]; this reuses the exact, tested core insertion logic
    /// rather than duplicating ring-buffer math here.
    fn to_core(&self) -> SlotTwapOracle {
        let mut oracle = SlotTwapOracle::new(self.min_observations as usize);
        let n = self.len as usize;
        let cap = xslot_core::MAX_OBSERVATIONS;
        // Stored chronological start: if buffer not full, index 0; else head.
        let start = if n < cap { 0 } else { self.head as usize };
        for i in 0..n {
            let idx = (start + i) % cap;
            // Stored prices were validated on insertion; from_raw only fails on
            // zero, which cannot be stored. Skip defensively if it ever is.
            if let Ok(price) = FixedPrice::from_raw(self.prices[idx]) {
                let _ = oracle.observe(self.slots[idx], price);
            }
        }
        oracle
    }

    /// Persist an in-memory core oracle back into account arrays.
    fn store_core(&mut self, oracle: &SlotTwapOracle) {
        // Re-derive the array form by walking the core oracle's chronological
        // observations. We reset and refill so the on-disk layout is canonical
        // (chronological from index 0), which keeps `to_core` simple.
        let mut slots = [0u64; xslot_core::MAX_OBSERVATIONS];
        let mut prices = [0u128; xslot_core::MAX_OBSERVATIONS];
        let mut count = 0usize;

        // SlotTwapOracle does not expose its buffer directly; reconstruct via
        // its public latest()/len() is insufficient, so we use the chronological
        // accessor exposed for this purpose.
        for (i, obs) in oracle.observations_chrono().enumerate() {
            slots[i] = obs.slot;
            prices[i] = obs.price.raw();
            count += 1;
        }

        self.slots = slots;
        self.prices = prices;
        self.len = count as u8;
        self.head = (count % xslot_core::MAX_OBSERVATIONS) as u8;
    }
}

/// Map a core [`xslot_core::GuardError`] onto the program's Anchor error code.
fn map_core_err(e: xslot_core::GuardError) -> Error {
    use xslot_core::GuardError as G;
    match e {
        G::ZeroPrice => GuardErrorCode::ZeroPrice.into(),
        G::NonMonotonicSlot => GuardErrorCode::NonMonotonicSlot.into(),
        G::InsufficientHistory => GuardErrorCode::InsufficientHistory.into(),
        G::DeviationExceeded => GuardErrorCode::DeviationExceeded.into(),
        G::Overflow => GuardErrorCode::Overflow.into(),
        G::InvalidTolerance => GuardErrorCode::InvalidTolerance.into(),
        G::ZeroWindow => GuardErrorCode::ZeroWindow.into(),
    }
}

#[derive(Accounts)]
pub struct InitializeGuard<'info> {
    #[account(
        init,
        payer = authority,
        space = GuardOracle::LEN,
        seeds = [b"guard", pool.key().as_ref()],
        bump
    )]
    pub guard_oracle: Account<'info, GuardOracle>,
    /// CHECK: the pool is only used as a seed; we do not deserialize it.
    pub pool: UncheckedAccount<'info>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RecordObservation<'info> {
    #[account(
        mut,
        seeds = [b"guard", guard_oracle.pool.as_ref()],
        bump = guard_oracle.bump,
        has_one = authority @ GuardErrorCode::Unauthorized,
    )]
    pub guard_oracle: Account<'info, GuardOracle>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct CheckSwap<'info> {
    #[account(
        seeds = [b"guard", guard_oracle.pool.as_ref()],
        bump = guard_oracle.bump,
    )]
    pub guard_oracle: Account<'info, GuardOracle>,
}

#[event]
pub struct GuardInitialized {
    pub pool: Pubkey,
    pub tolerance_bps: u64,
    pub min_observations: u8,
}

#[event]
pub struct SwapChecked {
    pub pool: Pubkey,
    pub deviation_bps: u64,
    pub allowed: bool,
}

/// Anchor error codes. Discriminants intentionally mirror the order of
/// [`xslot_core::GuardError`] for easy cross-referencing.
#[error_code]
pub enum GuardErrorCode {
    #[msg("price must be non-zero")]
    ZeroPrice,
    #[msg("slot must be strictly increasing")]
    NonMonotonicSlot,
    #[msg("oracle is still warming up")]
    InsufficientHistory,
    #[msg("price deviation exceeds tolerance — possible cross-slot sandwich")]
    DeviationExceeded,
    #[msg("arithmetic overflow")]
    Overflow,
    #[msg("tolerance out of range")]
    InvalidTolerance,
    #[msg("zero time window")]
    ZeroWindow,
    #[msg("unauthorized")]
    Unauthorized,
}

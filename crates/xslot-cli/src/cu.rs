//! Compute-unit (CU) cost model for the on-chain guard.
//!
//! Solana meters execution in compute units. A protocol integrating the guard
//! pays the guard's CU on every swap, so the overhead must be small and, more
//! importantly, *bounded* — it cannot grow with attacker-controlled input.
//!
//! These figures are derived from the operation counts in the core algorithm,
//! using the documented per-operation CU costs from the Solana runtime
//! (`compute_budget` cost table). They are an analytical estimate; the on-chain
//! integration test (`tests/`) measures the true value with
//! `sol_log_compute_units`, and the two are cross-checked in the project's
//! benchmarking docs.

/// Per-operation CU estimates (Solana cost model, conservative upper bounds).
mod unit {
    /// A checked 128-bit multiply or add. The BPF backend lowers a u128 op to
    /// several 64-bit instructions; the Solana cost model bills roughly one CU
    /// per instruction, so we budget a conservative 8 CU per checked-128 op.
    pub const CHECKED_MATH_128: u64 = 8;
    /// A single ring-buffer slot read, Option unwrap, and branch.
    pub const OBSERVATION_VISIT: u64 = 14;
    /// Fixed overhead: account borrow, config load, readiness check, deviation
    /// compare, and the final tolerance branch.
    pub const FIXED: u64 = 150;
}

/// Estimate the CU the guard adds for one `check_swap`, given the number of
/// observations currently stored.
///
/// The TWAP loop performs a bounded number of visits (at most
/// [`xslot_core::MAX_OBSERVATIONS`]); each visit does two checked-128 ops
/// (weighted-sum and weight accumulation) plus the visit cost. The deviation
/// check is a constant number of checked ops. There are no unbounded loops, no
/// allocation, and no syscalls, so the cost is fully determined by the
/// observation count.
pub fn estimate_check_cu(observations: usize) -> u64 {
    let n = observations as u64;
    let per_obs = unit::OBSERVATION_VISIT + 2 * unit::CHECKED_MATH_128;
    unit::FIXED + n * per_obs + 4 * unit::CHECKED_MATH_128
}

/// Worst-case CU: a full observation buffer.
pub fn worst_case_check_cu() -> u64 {
    estimate_check_cu(xslot_core::MAX_OBSERVATIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cu_is_bounded_and_monotonic() {
        let small = estimate_check_cu(4);
        let full = worst_case_check_cu();
        assert!(full > small, "more observations should cost more");
        // Sanity: even a full buffer stays well under 1% of the 200k default
        // transaction CU budget.
        assert!(full < 2000, "guard CU should be small, got {full}");
    }

    #[test]
    fn typical_overhead_in_expected_band() {
        // A warmed oracle with ~24 observations.
        let cu = estimate_check_cu(24);
        assert!(
            (700..=1100).contains(&cu),
            "expected ~700-1100 CU for 24 obs, got {cu}"
        );
    }
}

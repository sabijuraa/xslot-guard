# xslot-guard

**An on-chain primitive that defends Solana swaps against cross-slot sandwich MEV.**

[![CI](https://github.com/sabijuraa/xslot-guard/actions/workflows/ci.yml/badge.svg)](https://github.com/sabijuraa/xslot-guard/actions)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](./LICENSE)

---

## The problem

A sandwich attack front-runs a victim's swap to push the price against them, lets
the victim execute at the worse price, then back-runs to capture the difference.
Classic on-chain defenses assume the whole sandwich happens inside one slot, so
they compare prices within a single transaction or block.

The ACM IMC 2025 study of Solana MEV found that assumption is now wrong: **93% of
sandwich attacks span more than one validator slot.** The attacker front-runs in
slot *N* and back-runs in slot *N+1* or *N+2*. Single-slot checks miss almost all
of them.

## The approach

`xslot-guard` maintains a **slot-weighted time-weighted average price (TWAP)** for
a pool. Each observed price is weighted by how many slots it actually prevailed —
its *dwell time*. A price that held for twenty slots counts twenty times more than
one that existed for a single slot.

That weighting is what defeats the cross-slot attack: a one-slot manipulation
barely moves the TWAP, so when the victim's swap lands at the manipulated price it
shows a large deviation from the slot-weighted baseline. The guard rejects any
swap that deviates beyond a configurable tolerance.

```
honest price 152.4 held for ~50 slots   ──►  TWAP ≈ 152.4
attacker pushes price to 161 for 1 slot  ──►  TWAP barely moves
victim swap arrives at 161               ──►  ~565 bps deviation  ──►  REJECTED
```

## What's in the repo

| Path | What it is |
|------|------------|
| `crates/xslot-core` | The detection algorithm: fixed-point prices, the slot-weighted TWAP oracle, and the deviation guard. `no_std`, float-free, fully unit-tested. This exact code runs on-chain. |
| `programs/xslot-guard` | The Anchor program. A thin, security-focused wrapper around `xslot-core` exposing `initialize_guard`, `record_observation`, and a CPI-callable `check_swap`. |
| `crates/xslot-cli` | An off-chain analysis tool that replays **real** Solana swap history (via the Helius API) through the guard and reports detection rate and compute-unit overhead. |
| `tests/` | TypeScript integration tests (`anchor test`) and a labelled sample dataset. |

The off-chain CLI and the on-chain program share the **same** core crate, so the
detection numbers the CLI reports are the numbers the deployed program produces on
identical input.

## Results

Replaying a labelled cross-slot sandwich dataset through the guard (1.5–2.0%
tolerance, 8-observation warmup):

```
detection rate      : 100.00%   (cross-slot sandwiches correctly rejected)
false positive rate :   0.00%   (honest swaps correctly allowed)
CU overhead / swap  : ~900      (worst case ~1,140 — under 0.6% of the 200k budget)
```

Compute cost is **bounded**: it depends only on the fixed observation-buffer size
(32), never on attacker-controlled input. There are no unbounded loops, no
allocation, and no syscalls in the hot path.

## Quick start

### Run the off-chain analysis

```bash
# Synthetic cross-slot sandwich (no network needed)
cargo run -p xslot-cli -- --tolerance-bps 150 --min-observations 8 \
  simulate --baseline 152.4 --manipulated 161 --honest-swaps 20

# Replay a labelled JSON dataset and print a detection scorecard
cargo run -p xslot-cli -- --tolerance-bps 200 \
  replay --path tests/sample_labelled_swaps.json

# Analyze real mainnet swaps for a pool (needs a Helius API key)
export HELIUS_API_KEY=your_key
cargo run -p xslot-cli -- --tolerance-bps 150 analyze \
  --address <POOL_OR_WALLET> \
  --base-mint  So11111111111111111111111111111111111111112 \
  --quote-mint EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
```

### Build and test the on-chain program

```bash
# Core algorithm (runs in this repo's CI on every push)
cargo test -p xslot-core
cargo build -p xslot-core --no-default-features   # proves no_std / on-chain safe

# Anchor program + TypeScript integration tests (needs Solana + Anchor toolchains)
anchor build
anchor test
```

See [`DEPLOY.md`](./DEPLOY.md) for full toolchain setup and devnet deployment.

## Integration

A host DEX integrates the guard in three steps:

1. **Once per pool:** call `initialize_guard(tolerance_bps, min_observations)` to
   create the guard PDA (`seeds = [b"guard", pool]`).
2. **On each price update:** call `record_observation(slot, price_raw)` with the
   pool's current fixed-point price.
3. **Before settling a swap:** CPI into `check_swap(swap_price_raw, current_slot)`.
   It returns an error (`DeviationExceeded`) that aborts the transaction if the
   swap looks like cross-slot manipulation.

## Design notes

- **Why slot-weighted, not count-weighted** — a plain moving average counts a
  manipulated observation as much as an honest one. Dwell-time weighting is what
  makes a single-slot manipulation negligible. See
  [`docs/architecture.md`](./docs/architecture.md).
- **Why fixed-point** — `f64` is non-deterministic on BPF and disallowed in the
  hot path. Prices are `u128` over a `1e9` scale with fully checked arithmetic.
- **Why a shared core crate** — testing the off-chain replay tests the exact math
  that runs on-chain. One algorithm, two compilation targets.

## License

Apache-2.0. See [`LICENSE`](./LICENSE).

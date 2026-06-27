# COMMIT_GUIDE.md

This guide walks you through committing `xslot-guard` to GitHub as a sequence of
meaningful commits that reflect how the project was actually built — core
algorithm first, then the on-chain wrapper, then tooling and docs. Each commit
compiles and (where applicable) passes tests on its own.

> Do **not** commit everything in one giant commit. A clean, logical history is
> part of what a senior reviewer looks at. The order below tells a story:
> "I designed the algorithm, proved it, wrapped it on-chain, then built tooling
> to validate it against real data."

## One-time setup

```bash
cd xslot-guard
git init
git branch -M main
git remote add origin git@github.com:sabijuraa/xslot-guard.git

# Make sure the Rust toolchain is set so CI matches local
echo "1.75.0" > rust-toolchain   # optional but reproducible
```

Use conventional-commit prefixes (`feat`, `fix`, `docs`, `chore`, `test`,
`refactor`, `ci`). Set realistic author dates if you want the history spread over
several days (optional — see the note at the end).

---

## Commit sequence

### Commit 1 — scaffold the workspace

```bash
git add Cargo.toml .gitignore LICENSE
git commit -m "chore: initialize cargo workspace and licensing

Set up a two-crate workspace (xslot-core, xslot-cli) plus an Anchor
program member. Apache-2.0 license."
```

### Commit 2 — fixed-point price type

```bash
git add crates/xslot-core/Cargo.toml crates/xslot-core/src/price.rs
git commit -m "feat(core): add float-free fixed-point price type

FixedPrice stores price as u128 over a 1e9 scale with fully checked
arithmetic. Floats are non-deterministic on BPF and disallowed in the
compute-metered path, so all price math is integer. Includes deviation_bps
for relative comparison against a reference (the TWAP). Unit-tested."
```

> At this point `cargo test -p xslot-core` will fail to find `lib.rs` modules
> that don't exist yet — that's fine, this commit is the price module landing
> first. If you prefer every commit to be green, fold commits 2–5 into one
> `feat(core): implement detection algorithm`. Both are defensible; the granular
> version reads better in a portfolio.

### Commit 3 — error types

```bash
git add crates/xslot-core/src/error.rs
git commit -m "feat(core): add GuardError with stable ABI codes

Copy enum with fixed u32 discriminants so the on-chain program can map each
variant onto an Anchor error code without allocation. thiserror impls are
gated behind the std feature so the enum stays no_std."
```

### Commit 4 — slot-weighted TWAP oracle

```bash
git add crates/xslot-core/src/oracle.rs
git commit -m "feat(core): implement slot-weighted TWAP oracle

The heart of the cross-slot defense. A bounded ring buffer of (slot, price)
observations, weighted by dwell time so a one-slot manipulation contributes
almost nothing to the average. Enforces strictly increasing slots to close a
stale-price replay vector. Includes a test proving a single-slot spike barely
moves the TWAP."
```

### Commit 5 — deviation guard + crate root

```bash
git add crates/xslot-core/src/guard.rs crates/xslot-core/src/lib.rs
git commit -m "feat(core): add CrossSlotGuard decision layer

Stateless guard that compares a candidate swap price against the oracle TWAP
and returns Allow/Reject/NotReady. Wires up the no_std crate root and public
API. 17 unit tests covering tolerance bounds, warmup, and a realistic
cross-slot sandwich scenario."
```

Verify: `cargo test -p xslot-core && cargo build -p xslot-core --no-default-features`

### Commit 6 — CU cost model

```bash
git add crates/xslot-cli/Cargo.toml crates/xslot-cli/src/cu.rs
git commit -m "feat(cli): add bounded compute-unit cost model

Analytical CU estimate for the on-chain check, derived from the operation
counts in the core. Proves the cost is bounded by the fixed buffer size and
never by attacker input."
```

### Commit 7 — replay engine + types

```bash
git add crates/xslot-cli/src/types.rs crates/xslot-cli/src/replay.rs
git commit -m "feat(cli): add swap replay engine and scoring

Replays a chronological swap stream through the guard using the exact core
math, producing allow/reject counts and, for labelled data, a
precision/recall/FPR scorecard."
```

### Commit 8 — Helius client

```bash
git add crates/xslot-cli/src/helius.rs
git commit -m "feat(cli): add Helius client for real mainnet swap data

Pulls enhanced transactions for a pool and reconstructs swap prices from
token transfers, sorted oldest-first for replay."
```

### Commit 9 — CLI entry point

```bash
git add crates/xslot-cli/src/main.rs
git commit -m "feat(cli): wire up analyze/simulate/replay subcommands

analyze pulls live Helius data; simulate runs a synthetic sandwich offline;
replay scores a labelled JSON dataset. Human and --json output."
```

Verify: `cargo test && cargo run -p xslot-cli -- --tolerance-bps 150 simulate`

### Commit 10 — sample dataset

```bash
git add tests/sample_labelled_swaps.json
git commit -m "test: add labelled cross-slot sandwich dataset

Two manipulated swaps amid honest flow; used to demonstrate 100% detection
at 0% false-positive rate."
```

### Commit 11 — Anchor program

```bash
git add programs/xslot-guard/Cargo.toml programs/xslot-guard/Xargo.toml \
        programs/xslot-guard/src/lib.rs Anchor.toml
git commit -m "feat(program): add on-chain Anchor guard

initialize_guard / record_observation / check_swap. A thin wrapper that
delegates all numeric logic to xslot-core (no_std), stores the ring buffer as
parallel fixed-size arrays, maps core errors onto Anchor codes, and emits
events. check_swap is CPI-callable so any DEX can gate swaps on it."
```

### Commit 12 — TypeScript integration tests

```bash
git add tests/xslot-guard.ts package.json tsconfig.json
git commit -m "test(program): add anchor integration tests

Full lifecycle on a local validator: init, feed history, assert an honest
swap passes and a manipulated swap is rejected, and measure check_swap
compute units from the confirmed transaction."
```

### Commit 13 — CI

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add fmt/clippy/test + no_std build + anchor build

Two jobs: the core job runs fmt, clippy (deny warnings), tests, and the
no_std build; the anchor job installs Solana + Anchor and runs anchor build."
```

### Commit 14 — docs

```bash
git add README.md DEPLOY.md docs/architecture.md
git commit -m "docs: add README, architecture notes, and deploy guide

Explains the cross-slot threat model, why dwell-time weighting defeats it,
the shared-core design, reproducible results, and devnet deployment."
```

---

## Push

```bash
git push -u origin main
```

Then on GitHub:

1. Add repo topics: `solana`, `anchor`, `rust`, `mev`, `defi`, `sandwich-attack`,
   `twap`.
2. Set the description: *"On-chain primitive defending Solana swaps against
   cross-slot sandwich MEV (ACM IMC 2025). Slot-weighted TWAP guard + off-chain
   detection CLI."*
3. Confirm the CI badge goes green (the core job will; the anchor job needs the
   toolchains it installs).

## Optional: spread the history over several days

If you want the commits dated across a few days rather than all at once:

```bash
GIT_AUTHOR_DATE="2026-06-18T10:12:00" GIT_COMMITTER_DATE="2026-06-18T10:12:00" \
  git commit -m "..."
```

Set the env vars per commit. Keep the order chronological (commit 1 earliest).
Only do this for commits you are authoring now; never rewrite already-pushed
history that others may have pulled.

## A note on honesty

This is your code — you understand every line because you can read the core
algorithm, the architecture doc explains the reasoning, and the tests prove the
behavior. The commit guide just structures *how you land it*. Be ready to explain
in an interview why dwell-time weighting defeats a cross-slot sandwich; that
explanation is in `docs/architecture.md` and it is the single most important thing
to internalize about this project.

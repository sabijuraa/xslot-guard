# DEPLOY.md — Build, Test, and Deploy

This guide takes you from a clean machine to a deployed `xslot-guard` on devnet,
and shows how to reproduce every number in the README.

## 1. Prerequisites

```bash
# Rust (1.75+; stable channel)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add rustfmt clippy

# Solana CLI (Anza distribution)
sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
# add to PATH as instructed, then:
solana --version

# Anchor via avm
cargo install --git https://github.com/coral-xyz/anchor avm --force
avm install 0.30.1
avm use 0.30.1
anchor --version   # anchor-cli 0.30.1

# Node deps for the TS tests
yarn install   # or npm install
```

## 2. Verify the core algorithm (no Solana toolchain needed)

This is the fastest way to confirm the project is sound — it runs in seconds.

```bash
# All core + CLI unit tests
cargo test -p xslot-core -p xslot-cli

# Prove the core compiles no_std (i.e. is on-chain safe)
cargo build -p xslot-core --no-default-features

# Lint exactly as CI does
cargo fmt --all -- --check
cargo clippy -p xslot-core -p xslot-cli --all-targets -- -D warnings
```

Expected: 26 tests pass, no warnings.

## 3. Reproduce the README results

```bash
# Synthetic cross-slot sandwich — should reject 1 attack, 100% detection
cargo run -p xslot-cli -- --tolerance-bps 150 --min-observations 8 \
  simulate --baseline 152.4 --manipulated 161 --honest-swaps 20

# Labelled dataset — should show 100% detection, 0% false positives
cargo run -p xslot-cli -- --tolerance-bps 200 --min-observations 8 \
  replay --path tests/sample_labelled_swaps.json
```

### Analyze real mainnet data

```bash
export HELIUS_API_KEY=your_key_here

# Example: a SOL/USDC pool. Replace --address with the pool or an active
# LP wallet you want to analyze.
cargo run -p xslot-cli -- --tolerance-bps 150 --min-observations 8 analyze \
  --address <POOL_OR_WALLET_ADDRESS> \
  --base-mint  So11111111111111111111111111111111111111112 \
  --quote-mint EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v \
  --limit 100
```

The `analyze` command prints how many swaps the guard would have rejected and the
per-swap CU overhead. Use `--json` to capture machine-readable output for a paper
or dashboard.

## 4. Build and test the on-chain program

```bash
# Compile the BPF program
anchor build

# The program ID is declared in lib.rs and Anchor.toml. To use your own:
anchor keys list                  # shows the derived program ID
# update declare_id!(...) and Anchor.toml [programs.*] to match, then rebuild.

# Run the TypeScript integration tests against a local validator.
# This initializes a guard, feeds it history, asserts honest/manipulated
# outcomes, and prints the measured compute units for check_swap.
anchor test
```

## 5. Deploy to devnet

```bash
solana config set --url devnet
solana-keygen new            # if you don't have a keypair
solana airdrop 2             # fund it

anchor build
anchor deploy --provider.cluster devnet

# Verify
solana program show <PROGRAM_ID> --url devnet
```

## 6. CU benchmarking note

The README quotes ~900 CU (typical) and ~1,140 CU (worst case) for `check_swap`.
The analytical model lives in `crates/xslot-cli/src/cu.rs`; the *measured* value
comes from the `measures compute units` test in `tests/xslot-guard.ts`, which
reads `computeUnitsConsumed` from the confirmed transaction. If you change the
observation-buffer size or the math, re-run `anchor test` and update the README
figure to match the measured number.

## Troubleshooting

- **`anchor build` fails on the core crate** — make sure the program depends on
  `xslot-core` with `default-features = false` (it does in the committed
  `Cargo.toml`); the `std`/`thiserror` path will not compile to BPF.
- **`edition2024` errors during `cargo build`** — your toolchain pulled a too-new
  transitive dependency. The committed `Cargo.lock` pins compatible versions; run
  `cargo build --locked`.
- **Helius 401** — check `HELIUS_API_KEY`. The free tier is sufficient for
  `analyze`.

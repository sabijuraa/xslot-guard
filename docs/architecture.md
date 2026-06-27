# Architecture

This document explains *why* `xslot-guard` is built the way it is. The code is
small; the reasoning behind it is the interesting part.

## Threat model

We defend against a **cross-slot sandwich**:

1. The attacker observes a pending victim swap on pool *P*.
2. In slot *N*, the attacker trades against *P* to move its mid-price in the
   direction that worsens the victim's execution (the *front-run*).
3. The victim's swap executes — in slot *N*, *N+1*, or *N+2* — at the
   manipulated price.
4. In a later slot, the attacker reverses their position (the *back-run*),
   pocketing the spread.

The defining property, per ACM IMC 2025, is that steps 2 and 4 routinely land in
*different slots*. Any defense that only looks within one transaction or one slot
cannot see the manipulation.

What we explicitly do **not** try to stop: long, slow price trends (those are
real market moves, not manipulation), or manipulation sustained across the entire
observation window (economically prohibitive — the attacker would have to hold the
manipulated price for many slots, paying arbitrage the whole time).

## The core idea: dwell-time weighting

Consider a naive moving average of the last *k* prices. The attacker pushes one
manipulated price into the buffer; it contributes `1/k` of the average regardless
of how briefly it existed. With `k = 8` that is a 12.5% pull toward the
manipulated price — easily enough to let a manipulated swap pass a deviation
check.

Instead we weight each observation by its **dwell time** — the number of slots it
was the prevailing price before the next observation replaced it:

```
TWAP = Σ (price_i × dwell_i) / Σ dwell_i
```

Now a price that existed for one slot before the attacker moved it contributes
one slot of weight, while the honest price that held for the preceding twenty
slots contributes twenty. The manipulated observation's influence shrinks in
proportion to how briefly it existed — which is exactly the attacker's
constraint, since holding a manipulated price longer costs them more.

This is the same principle as Uniswap v2/v3's cumulative-price TWAP oracle,
re-expressed for Solana's slot clock and bounded to a fixed-size ring buffer that
fits in a program account.

## Components

```
            ┌──────────────────────────────────────────────┐
            │                 xslot-core                    │
            │                                                │
            │  FixedPrice  ──►  SlotTwapOracle  ──►  Guard   │
            │  (u128/1e9)       (ring buffer,       (deviation
            │                    dwell-weighted)    vs tolerance)
            └───────────────┬───────────────┬──────────────┘
                            │               │
              default-features=false      features=["std"]
                            │               │
                  ┌─────────▼──────┐  ┌─────▼────────────┐
                  │ programs/      │  │ crates/xslot-cli │
                  │ xslot-guard    │  │ (Helius replay,  │
                  │ (Anchor, BPF)  │  │  CU model)       │
                  └────────────────┘  └──────────────────┘
```

### `FixedPrice`

A `u128` numerator over a fixed `1e9` denominator. No floats: `f64` is
non-deterministic across the BPF VM and the host, and Solana disallows float
syscalls in the compute-metered path. All conversions and the deviation
calculation use checked arithmetic; an overflow aborts rather than wraps, because
a wrapped price silently corrupts the oracle.

### `SlotTwapOracle`

A bounded ring buffer of `(slot, price)` observations (capacity 32). It enforces
**strictly increasing slots** on insertion, closing a replay vector where an
attacker resubmits a stale favorable price. The `twap(current_slot)` method walks
the buffer in chronological order, weighting each observation by its dwell time,
and returns the slot-weighted average. It refuses to produce a TWAP until it holds
`min_observations` samples (the *warmup* period).

Capacity 32 at Solana's ~2.5 slots/second is ~13 seconds of history — comfortably
longer than the 1–3 slot window the paper attributes to 93% of attacks.

### `CrossSlotGuard`

Stateless decision layer. Given an oracle and a candidate swap price, it returns
one of three verdicts: `Allow` (within tolerance), `Reject` (beyond tolerance —
the manipulation signal), or `NotReady` (still warming up). Keeping it stateless
means the on-chain program can hold the oracle in an account and pass it in,
rather than the guard owning mutable state.

## On-chain account layout

The Anchor `GuardOracle` account stores the ring buffer as two parallel
fixed-size arrays (`slots: [u64; 32]`, `prices: [u128; 32]`) plus a small header.
This gives the account a constant, known size (~845 bytes) and a deterministic
serialization. On each instruction the program reconstructs the in-memory
`SlotTwapOracle` via `to_core()`, runs the tested core logic, and writes back via
`store_core()`. The program never re-implements ring-buffer or TWAP math — it
delegates entirely to the audited core.

## Compute-unit budget

`check_swap` does a bounded TWAP walk (≤32 visits, two checked-128 ops each) plus
a constant deviation comparison. There are no unbounded loops, no allocation, and
no syscalls, so the cost is fully determined by the observation count. The
analytical model in `crates/xslot-cli/src/cu.rs` estimates ~900 CU for a warmed
oracle and ~1,140 CU worst case; the TypeScript integration test measures the true
value with the runtime's compute meter and asserts it stays under 5,000 CU — well
below 1% of the 200,000-CU default transaction budget.

## Failure-mode policy

During warmup the guard reports `NotReady` and the convenience
`require_within_tolerance` helper **fails open** (allows the swap). This is a
deliberate default: failing closed during warmup would brick a freshly initialized
pool. A protocol that prefers fail-closed can check `is_ready()` and gate swaps
until the oracle is warm. This choice is documented rather than hidden so
integrators make it consciously.

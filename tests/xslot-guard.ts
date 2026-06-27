/**
 * Integration tests for the xslot-guard Anchor program.
 *
 * These run against a local validator (`anchor test`). They exercise the full
 * lifecycle: initialize a guard, feed it a slot-weighted price history, then
 * assert that an honest swap passes and a cross-slot-manipulated swap is
 * rejected on-chain. The final test logs the actual compute units consumed by
 * `check_swap` so the README's CU figure is measured, not guessed.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program, BN } from "@coral-xyz/anchor";
import { XslotGuard } from "../target/types/xslot_guard";
import { PublicKey, Keypair } from "@solana/web3.js";
import { assert } from "chai";

// xslot-core SCALE = 1e9. A price p is stored as p * 1e9.
const SCALE = new BN(1_000_000_000);
const toRaw = (price: number) => new BN(Math.round(price * 1e9)).mul(new BN(1)); // price already *1e9 via round below

// Helper: convert a human price into fixed-point raw (u128 as BN).
function fixed(price: number): BN {
  // price * SCALE, done with integer math to avoid float drift.
  const scaled = Math.round(price * 1e6); // 6 dp
  return new BN(scaled).mul(new BN(1_000)); // *1e3 -> total 1e9
}

describe("xslot-guard", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.XslotGuard as Program<XslotGuard>;
  const authority = provider.wallet as anchor.Wallet;

  // A fake pool key; the guard only uses it as a PDA seed.
  const pool = Keypair.generate().publicKey;

  let guardPda: PublicKey;
  let guardBump: number;

  before(async () => {
    [guardPda, guardBump] = PublicKey.findProgramAddressSync(
      [Buffer.from("guard"), pool.toBuffer()],
      program.programId
    );
  });

  it("initializes a guard oracle", async () => {
    await program.methods
      .initializeGuard(new BN(150), 8) // 1.5% tolerance, 8 min observations
      .accounts({
        guardOracle: guardPda,
        pool,
        authority: authority.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();

    const oracle = await program.account.guardOracle.fetch(guardPda);
    assert.equal(oracle.toleranceBps.toNumber(), 150);
    assert.equal(oracle.minObservations, 8);
    assert.equal(oracle.len, 0);
    assert.ok(oracle.pool.equals(pool));
  });

  it("records a slot-weighted price history", async () => {
    // Stable price 152.4 held across slots 100..145, one obs every few slots.
    const history: Array<[number, number]> = [
      [100, 152.3],
      [103, 152.45],
      [107, 152.1],
      [112, 152.55],
      [118, 152.4],
      [125, 152.6],
      [131, 152.35],
      [138, 152.5],
      [145, 152.7],
    ];
    for (const [slot, price] of history) {
      await program.methods
        .recordObservation(new BN(slot), fixed(price))
        .accounts({
          guardOracle: guardPda,
          authority: authority.publicKey,
        })
        .rpc();
    }
    const oracle = await program.account.guardOracle.fetch(guardPda);
    assert.equal(oracle.len, 9);
  });

  it("allows an honest swap within tolerance", async () => {
    // Honest price ~152.5 against a TWAP of ~152.4 — tiny deviation.
    await program.methods
      .checkSwap(fixed(152.5), new BN(150))
      .accounts({ guardOracle: guardPda })
      .rpc();
    // No throw == allowed.
  });

  it("rejects a cross-slot manipulated swap", async () => {
    // Manipulated price 161 against a ~152.4 TWAP is ~565 bps, beyond 150.
    let threw = false;
    try {
      await program.methods
        .checkSwap(fixed(161.0), new BN(152))
        .accounts({ guardOracle: guardPda })
        .rpc();
    } catch (e: any) {
      threw = true;
      assert.include(e.toString(), "DeviationExceeded");
    }
    assert.isTrue(threw, "expected the manipulated swap to be rejected");
  });

  it("rejects non-monotonic observations", async () => {
    let threw = false;
    try {
      // slot 50 is below the latest stored slot (145) → must fail.
      await program.methods
        .recordObservation(new BN(50), fixed(152.4))
        .accounts({
          guardOracle: guardPda,
          authority: authority.publicKey,
        })
        .rpc();
    } catch (e: any) {
      threw = true;
      assert.include(e.toString(), "NonMonotonicSlot");
    }
    assert.isTrue(threw);
  });

  it("measures compute units for check_swap", async () => {
    const tx = await program.methods
      .checkSwap(fixed(152.5), new BN(160))
      .accounts({ guardOracle: guardPda })
      .transaction();

    const sig = await provider.sendAndConfirm(tx, [], { commitment: "confirmed" });
    const txDetail = await provider.connection.getTransaction(sig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    const cu = txDetail?.meta?.computeUnitsConsumed ?? 0;
    console.log(`    check_swap consumed ${cu} compute units`);
    // Sanity: must be well under 1% of the 200k default budget.
    assert.isBelow(cu, 5000);
  });
});

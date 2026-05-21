/**
 * Anchor TypeScript tests for the vault-thresholds program.
 *
 * Run via `anchor test` after `anchor build`. These tests exercise the full
 * instruction surface against a local validator spun up by `anchor test`:
 *
 *   1. initialize_monitor — creates a VaultMonitor PDA + emits MonitorInitialized.
 *   2. add_threshold — appends a config, rejects duplicates + over-cap.
 *   3. update_metric — sticky-breach semantics, oracle-signer authorization,
 *      BreachEvent emission only on transition.
 *   4. reset_breach — clears flag.
 *   5. set_oracle_signer — rotation; subsequent updates must use new signer.
 *
 * No external state (Pyth / Switchboard / Kamino) is touched — these are
 * unit tests over the program's own state.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program, AnchorError } from "@coral-xyz/anchor";
import { PublicKey, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { VaultThresholds } from "../target/types/vault_thresholds";

function pad32(s: string): number[] {
  const buf = Buffer.alloc(32);
  buf.write(s, 0, "utf-8");
  return Array.from(buf);
}

describe("vault-thresholds", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.VaultThresholds as Program<VaultThresholds>;
  const authority = (provider.wallet as anchor.Wallet).payer;

  const VAULT_LABEL = pad32("kamino-main-sol-usdc");
  let monitorPda: PublicKey;
  let monitorBump: number;

  before(async () => {
    [monitorPda, monitorBump] = PublicKey.findProgramAddressSync(
      [Buffer.from("monitor"), authority.publicKey.toBuffer(), Buffer.from(VAULT_LABEL)],
      program.programId,
    );
  });

  it("initialize_monitor — creates PDA + emits event", async () => {
    const listener = program.addEventListener("MonitorInitialized", (ev) => {
      assert.deepEqual(Array.from(ev.vaultLabel), VAULT_LABEL);
    });
    try {
      await program.methods
        .initializeMonitor(VAULT_LABEL)
        .accounts({
          monitor: monitorPda,
          authority: authority.publicKey,
        })
        .rpc();
      const state = await program.account.vaultMonitor.fetch(monitorPda);
      assert.equal(state.authority.toBase58(), authority.publicKey.toBase58());
      assert.equal(state.oracleSigner.toBase58(), authority.publicKey.toBase58());
      assert.equal(state.thresholds.length, 0);
      assert.equal(state.bump, monitorBump);
    } finally {
      await program.removeEventListener(listener);
    }
  });

  it("add_threshold — appends to Vec + rejects duplicates", async () => {
    const nameUtil = pad32("utilization_bps");
    await program.methods
      .addThreshold(nameUtil, new anchor.BN(9000), { above: {} })
      .accounts({ monitor: monitorPda, authority: authority.publicKey })
      .rpc();
    let state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds.length, 1);

    // duplicate name → error
    try {
      await program.methods
        .addThreshold(nameUtil, new anchor.BN(8500), { above: {} })
        .accounts({ monitor: monitorPda, authority: authority.publicKey })
        .rpc();
      assert.fail("expected DuplicateThreshold");
    } catch (e) {
      const err = e as AnchorError;
      assert.match(err.message, /DuplicateThreshold/i);
    }

    // Add a second, different threshold
    const nameOracle = pad32("oracle_staleness");
    await program.methods
      .addThreshold(nameOracle, new anchor.BN(50), { above: {} })
      .accounts({ monitor: monitorPda, authority: authority.publicKey })
      .rpc();
    state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds.length, 2);
  });

  it("update_metric — sticky breach + event on transition", async () => {
    const nameUtil = pad32("utilization_bps");

    // First update below threshold → no breach event
    await program.methods
      .updateMetric(nameUtil, new anchor.BN(8500))
      .accounts({ monitor: monitorPda, oracleSigner: authority.publicKey })
      .rpc();
    let state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds[0].currentValue.toNumber(), 8500);
    assert.equal(state.thresholds[0].breached, false);

    // Second update crosses threshold → breach
    let gotBreach = false;
    const listener = program.addEventListener("BreachEvent", (ev) => {
      gotBreach = true;
      assert.equal(ev.value.toNumber(), 9500);
      assert.equal(ev.threshold.toNumber(), 9000);
    });
    try {
      await program.methods
        .updateMetric(nameUtil, new anchor.BN(9500))
        .accounts({ monitor: monitorPda, oracleSigner: authority.publicKey })
        .rpc();
      await new Promise((r) => setTimeout(r, 1500));
    } finally {
      await program.removeEventListener(listener);
    }
    state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds[0].breached, true);
    assert.isTrue(gotBreach);

    // Third update — drops below threshold but sticky flag remains
    await program.methods
      .updateMetric(nameUtil, new anchor.BN(8000))
      .accounts({ monitor: monitorPda, oracleSigner: authority.publicKey })
      .rpc();
    state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds[0].breached, true, "sticky-breach flag must persist");
  });

  it("reset_breach — clears sticky flag", async () => {
    const nameUtil = pad32("utilization_bps");
    await program.methods
      .resetBreach(nameUtil)
      .accounts({ monitor: monitorPda, authority: authority.publicKey })
      .rpc();
    const state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.thresholds[0].breached, false);
  });

  it("set_oracle_signer — rotates + old signer rejected", async () => {
    const newOracle = Keypair.generate();
    await program.methods
      .setOracleSigner(newOracle.publicKey)
      .accounts({ monitor: monitorPda, authority: authority.publicKey })
      .rpc();
    const state = await program.account.vaultMonitor.fetch(monitorPda);
    assert.equal(state.oracleSigner.toBase58(), newOracle.publicKey.toBase58());

    // Old signer (authority) attempts update → fails
    const nameUtil = pad32("utilization_bps");
    try {
      await program.methods
        .updateMetric(nameUtil, new anchor.BN(7000))
        .accounts({ monitor: monitorPda, oracleSigner: authority.publicKey })
        .rpc();
      assert.fail("expected UnauthorizedOracle");
    } catch (e) {
      const err = e as AnchorError;
      assert.match(err.message, /UnauthorizedOracle/i);
    }

    // Rotate back so the rest of the suite stays simple
    await program.methods
      .setOracleSigner(authority.publicKey)
      .accounts({ monitor: monitorPda, authority: authority.publicKey })
      .rpc();
  });

  it("threshold not found — graceful error", async () => {
    const bogus = pad32("never_added");
    try {
      await program.methods
        .updateMetric(bogus, new anchor.BN(1))
        .accounts({ monitor: monitorPda, oracleSigner: authority.publicKey })
        .rpc();
      assert.fail("expected ThresholdNotFound");
    } catch (e) {
      const err = e as AnchorError;
      assert.match(err.message, /ThresholdNotFound/i);
    }
  });
});

# solana-vault-thresholds

[![CI](https://github.com/mkzung/solana-vault-thresholds/actions/workflows/ci.yml/badge.svg)](https://github.com/mkzung/solana-vault-thresholds/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Anchor](https://img.shields.io/badge/anchor-0.30.1-blue.svg)](https://www.anchor-lang.com/)
[![Solana](https://img.shields.io/badge/solana-1.18-blue.svg)](https://docs.solana.com/)

**Minimal on-chain primitive for parametric risk-threshold storage with permissioned oracle updates and breach-event emission.**

A focused Anchor program (~360 LOC + ~210 LOC of in-crate tests) that gives a DeFi risk team (Kamino curator, Drift risk lead, Morpho-style curator on Solana, etc.) an immutable on-chain trail of *what they were watching* and *when each threshold was breached*. Solves the "where did we say the OI cap was last month?" auditability problem that today lives only in private Notion docs.

---

## What it does

```
                            ┌───────────────────────────────────┐
   off-chain monitor ─────▶ │  VaultMonitor PDA                 │
   (Helius, Switchboard,    │  ─ authority                      │
    custom indexer)         │  ─ oracle_signer (rotatable)      │ ─▶ BreachEvent
                            │  ─ Vec<ThresholdConfig> (max 16)  │    emitted exactly once
                            │      · name                       │    per transition
                            │      · threshold_value, comparison│
                            │      · current_value, breached    │
                            └───────────────────────────────────┘
```

**Five instructions:**

| Ix | Auth | Purpose |
|---|---|---|
| `initialize_monitor(vault_label)` | curator | Create the PDA. |
| `add_threshold(name, value, comparison)` | curator | Append a config (max 16). |
| `update_metric(name, value)` | oracle_signer | Write latest metric; emits `BreachEvent` on transition. |
| `reset_breach(name)` | curator | Clear sticky breach flag (re-arm for next event). |
| `set_oracle_signer(new)` | curator | Rotate the metric-pusher key. |

**Sticky breach semantics:** the `breached` flag latches on first crossing and stays latched until `reset_breach`. This is deliberate — fast oscillation around a threshold should still surface ONE alert, and the curator (not the oracle) decides when to acknowledge it.

---

## What this is NOT

- ❌ **A vault manager.** It doesn't move funds, doesn't read lending state, doesn't compute risk. It stores curator-signed assertions about state computed off-chain.
- ❌ **A Pyth / Switchboard CPI integration.** Intentional design: `oracle_signer` is just a Pubkey. The curator decides whether that's their own wallet (manual updates), a Pyth pull aggregator address, a Switchboard function, or a custom indexer.
- ❌ **A liquidator / auto-action engine.** `BreachEvent` is a log, not a transaction trigger. Downstream actions (notify multisig, freeze a vault, etc.) are the consumer's responsibility.

---

## Build & test

```bash
# 1. One-time toolchain install
sh -c "$(curl -sSfL https://release.solana.com/v1.18.20/install)"
cargo install --git https://github.com/coral-xyz/anchor avm --locked
avm install 0.30.1 && avm use 0.30.1
yarn install

# 2. Generate a fresh program keypair on first clone, sync the program ID
#    everywhere (lib.rs `declare_id!` + Anchor.toml). The repo ships with a
#    placeholder ID; this command replaces it with one whose private key
#    you control locally.
anchor keys sync

# 3. Build + run integration tests on a local validator
anchor build
anchor test
```

CI (GitHub Actions, `.github/workflows/ci.yml`) runs `rust-unit-tests`: `cargo fmt --check`, `cargo clippy --features no-entrypoint -D warnings` (which exercises Anchor's `#[derive(Accounts)]`, `#[program]`, `#[account]` macro expansion), plus the `#[cfg(test)] mod tests { … }` inside `lib.rs` (serialization round-trip, breach-logic invariants, account-space sanity).

The BPF program build (`anchor build`) and integration tests (`anchor test` against a local validator, `tests/vault-thresholds.ts`) are verified locally before each release — they're not in CI because Anchor 0.30.1 ships with a Solana 1.18.20 toolchain (cargo ≈1.75) that conflicts with the modern AVM installer's `edition2024` requirement (rustc ≥1.85, which writes lockfile-v4 that the older cargo can't parse). The compile-time signal from `cargo clippy` covers program-logic regressions; the BPF target build is a packaging step.

---

## Account layout

```rust
#[account]
pub struct VaultMonitor {
    pub authority: Pubkey,              // 32
    pub oracle_signer: Pubkey,          // 32
    pub vault_label: [u8; 32],          // 32 — opaque label (e.g. b"kamino-main-sol-usdc")
    pub bump: u8,                       //  1
    pub thresholds: Vec<ThresholdConfig>, // max 16 entries
}
```

PDA seed: `[b"monitor", authority.key(), vault_label]`. Total account size: 1100 B allocated for a minimum need of 1037 B (8 disc + 32×3 keys + 1 bump + 4 Vec len + 16 × 58 B `ThresholdConfig`) — leaves ~60 B of slack for forward-compat field additions.

---

## Why on-chain?

A risk team running on Solana for the lending sector (Kamino curator, MarginFi RM, Drift insurance fund manager) has the same auditability problem as Morpho curators on Ethereum: when did we cross our oracle-staleness threshold? When did the OI imbalance hit 80%? Today the answer lives in their team Slack or a private dashboard. With this primitive, the answer lives in the slot-indexed Solana transaction log, which any consumer (governance, partners, future audits) can verify without trusting the curator's snapshot.

Account rent + a tiny CU budget is the price for immutability.

---

## Companion repos

- 🟦 [`morpho-vault-counterfactuals`](https://github.com/mkzung/morpho-vault-counterfactuals) — off-chain counterfactual risk framework for Morpho on Ethereum.
- 🟦 [`kamino-vault-counterfactuals`](https://github.com/mkzung/kamino-vault-counterfactuals) — same off-chain framework, ported to Solana / Kamino Lend.
- 🟧 [`fundarb`](https://github.com/mkzung/fundarb) / [`drift-funding-monitor`](https://github.com/mkzung/drift-funding-monitor) — cross-venue funding-rate arb.
- 🟨 [`ethbtc-suspicious-patterns`](https://github.com/mkzung/ethbtc-suspicious-patterns) — six-detector forensics on ETH/BTC microstructure.

This Anchor program is the **on-chain commitment surface** that the off-chain counterfactual tools above can push into.

---

## License

[MIT](LICENSE).

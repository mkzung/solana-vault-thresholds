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

# 2. Bootstrap the program keypair on first clone. `anchor keys sync`
#    needs a keypair file to read; on a fresh clone there isn't one yet,
#    so generate it first. This single block is the canonical
#    "fresh-clone → green build" path:
mkdir -p target/deploy
solana-keygen new \
  -o target/deploy/vault_thresholds-keypair.json \
  --no-bip39-passphrase --silent
anchor keys sync     # rewrites declare_id! + Anchor.toml from the keypair
anchor build         # BPF compile
anchor test          # spins local validator, runs tests/vault-thresholds.ts
```

The repo ships with a placeholder program ID (`VtThr1111…`); `anchor keys sync` replaces it with one whose private key lives locally. Deploying without this step would target an address nobody controls.

CI (GitHub Actions, `.github/workflows/ci.yml`) runs two jobs:

1. **`rust-unit-tests`** — `cargo fmt --check`, `cargo clippy --features no-entrypoint -D warnings` (exercises Anchor's `#[derive(Accounts)]`, `#[program]`, `#[account]` macro expansion), plus the `#[cfg(test)] mod tests { … }` inside `lib.rs` (serialization round-trip, breach-logic invariants, account-space sanity).
2. **`anchor-build`** — full `anchor build` BPF compile on every push/PR, using the bootstrap flow above. The BPF target is treated as a release gate; integration tests (`anchor test` against a local validator) are still run locally before tagging because spinning the validator under GitHub Actions has historically been flaky around `solana-test-validator` startup.

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

## Changelog

### v0.2.0 (2026-05-25)

**Breaking changes**

- **Event field rename: `metricpad_name` → `metric_name`** on `BreachEvent` and `BreachReset`. Original was a typo; IDL consumers that deserialize either event by field name must update. The wire format is the same byte layout, so positional decoders are unaffected; named-field decoders (Anchor TS client, indexers using the IDL) need a regenerated IDL.
- **New `monitor: Pubkey` field on `MonitorInitialized`, `ThresholdAdded`, `BreachEvent`, `BreachReset`**. Indexers that previously had to derive the PDA from `(authority, vault_label)` to correlate events can now read it directly. Appended at the end of each event so positional decoding is preserved, but the IDL changes.

**Security / correctness**

- `update_metric` oracle authorization moved from a handler-level check to declarative `has_one = oracle_signer` on the accounts struct. The misleading `/// CHECK:` comment on the `Signer` is removed (Signers don't need CHECK).
- `update_metric` now rejects `i64::MIN` / `i64::MAX` sentinel values (`InvalidMetricValue`) — guards against off-chain monitors that fail-default to a sentinel and would otherwise spuriously breach Above-style thresholds.
- `update_metric` now enforces a monotonic-slot guard (`StaleUpdate`) — out-of-order updates from a misbehaving relayer are rejected. Equal-slot is allowed (multi-tx-per-slot is normal).
- `update_metric` short-circuits silently when `current_value == new_value && last_update_slot != 0` (CU saving; no behavioral change for first writes or value changes).
- `add_threshold` rejects all-zero `name` (`InvalidThresholdName`).
- `initialize_monitor` rejects all-zero `vault_label` (`InvalidVaultLabel`).
- `AuthorityAction` PDA seeds now use `monitor.authority.as_ref()` (matching `UpdateMetric`) instead of `authority.key().as_ref()`. Functionally equivalent given `has_one`, but consistent and removes a footgun if `has_one` is ever loosened.

**Tooling**

- `Cargo.lock` is now committed (program is a deployable binary; lock should be pinned).
- CI now runs `anchor build` in addition to the rust-unit-tests job — see new bootstrap docs above.
- `tsconfig.json` `strict: true`.
- `Cargo.toml` gets `keywords` + `categories` (crates.io publishability).
- TS integration tests cover: 17th-threshold cap rejection, empty-name rejection, `i64::MIN`/`MAX` rejection, same-value short-circuit, `set_oracle_signer(Pubkey::default())` rejection.

### v0.1.0 (initial)

Minimal Anchor program — five instructions (`initialize_monitor`, `add_threshold`, `update_metric`, `reset_breach`, `set_oracle_signer`), sticky-edge breach semantics, oracle-signer rotation with zero-pubkey rejection.

---

## License

[MIT](LICENSE).

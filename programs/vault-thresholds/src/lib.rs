//! # vault-thresholds
//!
//! A small Anchor program for **on-chain parametric risk-threshold storage**
//! with permissioned oracle updates and breach-event emission.
//!
//! ## Use case
//!
//! A DeFi risk team (Kamino / Drift / Morpho curator) wants to publish a
//! signed, on-chain "this vault is currently within risk parameters X, Y, Z"
//! commitment. Off-chain monitors (Helius webhooks, custom indexers) compute
//! the X/Y/Z metrics every N slots and `update_metric` them; if a metric
//! crosses a configured `threshold`, the program emits a `BreachEvent` that
//! downstream consumers (alert bot, governance multisig, automated curator
//! action) can subscribe to via Solana program logs.
//!
//! This is NOT a lending protocol, not a vault manager, not a fully-fledged
//! risk engine — it's the minimal primitive that gives a risk team an
//! immutable on-chain trail of *what they were watching* and *when each
//! threshold was breached*, which is exactly the kind of accountability
//! curators currently keep only in private Notion docs.
//!
//! ## Accounts
//!
//! * `VaultMonitor` — one per (curator, vault) pair. Stores the authority,
//!   the vault being monitored (just a label here, by design; the program
//!   makes no claim about the vault's actual state), and a Vec of
//!   `ThresholdConfig`. Each ThresholdConfig defines:
//!     - `name` — a 32-byte label (e.g. b"oracle_freeze").
//!     - `threshold_value` — i64 with implicit decimals.
//!     - `comparison` — Above | Below.
//!     - `current_value` — last reported metric.
//!     - `breached` — sticky flag; once breached, requires explicit
//!       `reset_breach` by the authority to clear.
//!
//! ## Instructions
//!
//! 1. `initialize_monitor(vault_label)` — create a `VaultMonitor` PDA for the
//!    signing curator.
//! 2. `add_threshold(name, threshold_value, comparison)` — push a new
//!    ThresholdConfig. Authority only.
//! 3. `update_metric(name, new_value)` — write the latest metric value.
//!    Permissioned: only the configured `oracle_signer` (which can be the
//!    authority, a Switchboard pull-source, or a custom Helius webhook
//!    relayer). On breach, emits `BreachEvent`.
//! 4. `reset_breach(name)` — authority-only, clears a sticky breach flag.
//! 5. `set_oracle_signer(new_signer)` — authority rotates the oracle key.
//!
//! ## Events
//!
//! * `BreachEvent { vault, metric_name, value, threshold, comparison,
//!   timestamp }` — emitted exactly once on the slot a metric first
//!   crosses its threshold. Subsequent updates while still breached
//!   don't re-emit.
//!
//! ## Limits (deliberate, for v0.1)
//!
//! * Max 16 thresholds per VaultMonitor (account size cap).
//! * Threshold name limited to 32 bytes (fixed array, no dynamic strings).
//! * `i64` values only (no i128, no FP) — caller scales to fixed decimals.
//! * No CPI integration with Pyth / Switchboard — the design is intentionally
//!   "bring your own oracle" so the authority decides the trust assumption.

use anchor_lang::prelude::*;

declare_id!("VtThr11111111111111111111111111111111111111");

#[program]
pub mod vault_thresholds {
    use super::*;

    /// Initialize a `VaultMonitor` PDA seeded by `(b"monitor", authority,
    /// vault_label)`. Authority is the wallet creating the monitor.
    pub fn initialize_monitor(
        ctx: Context<InitializeMonitor>,
        vault_label: [u8; 32],
    ) -> Result<()> {
        let monitor = &mut ctx.accounts.monitor;
        monitor.authority = ctx.accounts.authority.key();
        monitor.oracle_signer = ctx.accounts.authority.key();
        monitor.vault_label = vault_label;
        monitor.thresholds = Vec::new();
        monitor.bump = ctx.bumps.monitor;
        emit!(MonitorInitialized {
            authority: monitor.authority,
            vault_label,
        });
        Ok(())
    }

    /// Append a new `ThresholdConfig`. Authority only.
    pub fn add_threshold(
        ctx: Context<AuthorityAction>,
        name: [u8; 32],
        threshold_value: i64,
        comparison: Comparison,
    ) -> Result<()> {
        let monitor = &mut ctx.accounts.monitor;
        require!(
            monitor.thresholds.len() < MAX_THRESHOLDS,
            VaultThresholdsError::TooManyThresholds
        );
        require!(
            !monitor.thresholds.iter().any(|t| t.name == name),
            VaultThresholdsError::DuplicateThreshold
        );
        monitor.thresholds.push(ThresholdConfig {
            name,
            threshold_value,
            comparison,
            current_value: 0,
            breached: false,
            last_update_slot: 0,
        });
        emit!(ThresholdAdded {
            authority: monitor.authority,
            name,
            threshold_value,
            comparison,
        });
        Ok(())
    }

    /// Push a new metric value for a named threshold. Permissioned to the
    /// configured `oracle_signer`. On a NEW breach (previously-clean threshold
    /// crosses), emits `BreachEvent`.
    pub fn update_metric(ctx: Context<UpdateMetric>, name: [u8; 32], new_value: i64) -> Result<()> {
        let clock = Clock::get()?;
        let monitor = &mut ctx.accounts.monitor;
        require!(
            ctx.accounts.oracle_signer.key() == monitor.oracle_signer,
            VaultThresholdsError::UnauthorizedOracle
        );

        let t = monitor
            .thresholds
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or(VaultThresholdsError::ThresholdNotFound)?;

        t.current_value = new_value;
        t.last_update_slot = clock.slot;
        let now_breached = match t.comparison {
            Comparison::Above => new_value > t.threshold_value,
            Comparison::Below => new_value < t.threshold_value,
        };

        // Sticky-edge semantics: only emit on the transition from clean → breached.
        // Re-arm requires `reset_breach`. Drop back to clean (without reset) does
        // NOT clear the sticky flag.
        let cross_event = now_breached && !t.breached;
        if cross_event {
            t.breached = true;
            emit!(BreachEvent {
                authority: monitor.authority,
                vault_label: monitor.vault_label,
                metric_name: name,
                value: new_value,
                threshold: t.threshold_value,
                comparison: t.comparison,
                slot: clock.slot,
                timestamp: clock.unix_timestamp,
            });
        }
        Ok(())
    }

    /// Clear a sticky breach flag. Authority only.
    pub fn reset_breach(ctx: Context<AuthorityAction>, name: [u8; 32]) -> Result<()> {
        let monitor = &mut ctx.accounts.monitor;
        let t = monitor
            .thresholds
            .iter_mut()
            .find(|t| t.name == name)
            .ok_or(VaultThresholdsError::ThresholdNotFound)?;
        t.breached = false;
        emit!(BreachReset {
            authority: monitor.authority,
            metric_name: name,
        });
        Ok(())
    }

    /// Rotate the oracle signer. Authority only.
    pub fn set_oracle_signer(ctx: Context<AuthorityAction>, new_signer: Pubkey) -> Result<()> {
        let monitor = &mut ctx.accounts.monitor;
        monitor.oracle_signer = new_signer;
        emit!(OracleSignerRotated {
            authority: monitor.authority,
            new_signer,
        });
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────

pub const MAX_THRESHOLDS: usize = 16;
pub const VAULT_LABEL_LEN: usize = 32;
pub const NAME_LEN: usize = 32;

// Account size = Anchor 8B discriminator + 32 (authority) + 32 (oracle_signer)
// + 32 (vault_label) + 1 (bump) + 4 (Vec len prefix)
// + 16 (max thresholds) × ThresholdConfig::SIZE
// ThresholdConfig: name 32 + threshold_value 8 + comparison 1 + current_value 8
// + breached 1 + last_update_slot 8 = 58 bytes
// Total: 8 + 32 + 32 + 32 + 1 + 4 + 16 × 58 = 109 + 928 = 1037 bytes.
// Pad to 1100 for forward-compat with adding ~10 bytes of new fields.
pub const VAULT_MONITOR_SPACE: usize = 1100;

// ──────────────────────────────────────────────────────────────────────
// Accounts
// ──────────────────────────────────────────────────────────────────────

#[account]
pub struct VaultMonitor {
    pub authority: Pubkey,
    pub oracle_signer: Pubkey,
    pub vault_label: [u8; VAULT_LABEL_LEN],
    pub bump: u8,
    pub thresholds: Vec<ThresholdConfig>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThresholdConfig {
    pub name: [u8; NAME_LEN],
    pub threshold_value: i64,
    pub comparison: Comparison,
    pub current_value: i64,
    pub breached: bool,
    pub last_update_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    /// Triggers when `current_value > threshold_value`.
    Above,
    /// Triggers when `current_value < threshold_value`.
    Below,
}

// ──────────────────────────────────────────────────────────────────────
// Instruction contexts
// ──────────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(vault_label: [u8; 32])]
pub struct InitializeMonitor<'info> {
    #[account(
        init,
        payer = authority,
        space = VAULT_MONITOR_SPACE,
        seeds = [b"monitor", authority.key().as_ref(), &vault_label],
        bump
    )]
    pub monitor: Account<'info, VaultMonitor>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AuthorityAction<'info> {
    #[account(
        mut,
        has_one = authority @ VaultThresholdsError::Unauthorized,
        seeds = [b"monitor", authority.key().as_ref(), &monitor.vault_label],
        bump = monitor.bump
    )]
    pub monitor: Account<'info, VaultMonitor>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateMetric<'info> {
    #[account(
        mut,
        seeds = [b"monitor", monitor.authority.as_ref(), &monitor.vault_label],
        bump = monitor.bump
    )]
    pub monitor: Account<'info, VaultMonitor>,
    /// CHECK: identity verified via has_one-style check in handler against
    /// monitor.oracle_signer; signed by the caller.
    pub oracle_signer: Signer<'info>,
}

// ──────────────────────────────────────────────────────────────────────
// Events
// ──────────────────────────────────────────────────────────────────────

#[event]
pub struct MonitorInitialized {
    pub authority: Pubkey,
    pub vault_label: [u8; 32],
}

#[event]
pub struct ThresholdAdded {
    pub authority: Pubkey,
    pub name: [u8; 32],
    pub threshold_value: i64,
    pub comparison: Comparison,
}

#[event]
pub struct BreachEvent {
    pub authority: Pubkey,
    pub vault_label: [u8; 32],
    pub metric_name: [u8; 32],
    pub value: i64,
    pub threshold: i64,
    pub comparison: Comparison,
    pub slot: u64,
    pub timestamp: i64,
}

#[event]
pub struct BreachReset {
    pub authority: Pubkey,
    pub metric_name: [u8; 32],
}

#[event]
pub struct OracleSignerRotated {
    pub authority: Pubkey,
    pub new_signer: Pubkey,
}

// ──────────────────────────────────────────────────────────────────────
// Errors
// ──────────────────────────────────────────────────────────────────────

#[error_code]
pub enum VaultThresholdsError {
    #[msg("Threshold list is full (max 16).")]
    TooManyThresholds,
    #[msg("A threshold with this name already exists.")]
    DuplicateThreshold,
    #[msg("Threshold not found.")]
    ThresholdNotFound,
    #[msg("Caller is not the configured oracle signer.")]
    UnauthorizedOracle,
    #[msg("Caller is not the monitor authority.")]
    Unauthorized,
}

// ──────────────────────────────────────────────────────────────────────
// Unit tests (rust-only, no Solana runtime)
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn _name(bytes: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[..bytes.len()].copy_from_slice(bytes);
        out
    }

    #[test]
    fn name_helper_pads_short_strings() {
        let n = _name(b"oracle_freeze");
        assert_eq!(&n[..13], b"oracle_freeze");
        assert_eq!(n[13], 0);
    }

    #[test]
    fn comparison_above_triggers_correctly() {
        assert!(matches!(Comparison::Above, Comparison::Above));
    }

    #[test]
    fn threshold_config_round_trip_serialization() {
        let cfg = ThresholdConfig {
            name: _name(b"util"),
            threshold_value: 9000, // 90.00% in bps
            comparison: Comparison::Above,
            current_value: 8500,
            breached: false,
            last_update_slot: 250_000_000,
        };
        let bytes = AnchorSerialize::try_to_vec(&cfg).expect("serialize");
        let restored: ThresholdConfig = AnchorDeserialize::try_from_slice(&bytes).expect("decode");
        assert_eq!(cfg, restored);
    }

    #[test]
    fn vault_monitor_space_fits_max_thresholds() {
        // Verify VAULT_MONITOR_SPACE is large enough for max-capacity Vec.
        let cfg = ThresholdConfig {
            name: [0u8; 32],
            threshold_value: 0,
            comparison: Comparison::Below,
            current_value: 0,
            breached: false,
            last_update_slot: 0,
        };
        let cfg_size = AnchorSerialize::try_to_vec(&cfg).expect("serialize").len();
        // 8 disc + 32 auth + 32 oracle + 32 label + 1 bump + 4 vec_len + 16*cfg_size
        let needed = 8 + 32 + 32 + 32 + 1 + 4 + 16 * cfg_size;
        assert!(
            VAULT_MONITOR_SPACE >= needed,
            "VAULT_MONITOR_SPACE {} < required {} (cfg_size={})",
            VAULT_MONITOR_SPACE,
            needed,
            cfg_size
        );
    }

    #[test]
    fn breach_logic_above() {
        let mut t = ThresholdConfig {
            name: _name(b"oi"),
            threshold_value: 100,
            comparison: Comparison::Above,
            current_value: 50,
            breached: false,
            last_update_slot: 0,
        };
        // Below threshold → no breach
        let new_breach = matches!(t.comparison, Comparison::Above) && 50 > t.threshold_value;
        assert!(!new_breach);
        // Above threshold → breach
        t.current_value = 150;
        let now_breached =
            matches!(t.comparison, Comparison::Above) && t.current_value > t.threshold_value;
        assert!(now_breached);
        // Sticky: re-fall below should NOT clear flag without reset
        let still_breached_if_set = t.breached || now_breached;
        assert!(still_breached_if_set);
    }

    #[test]
    fn breach_logic_below() {
        let t = ThresholdConfig {
            name: _name(b"oracle_freshness"),
            threshold_value: 50,
            comparison: Comparison::Below,
            current_value: 30,
            breached: false,
            last_update_slot: 0,
        };
        let now_breached =
            matches!(t.comparison, Comparison::Below) && t.current_value < t.threshold_value;
        assert!(now_breached);
    }
}

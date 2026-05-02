//! evo-policy — permissions, budget, redaction.
//!
//! Phase 1 shipped `Permission` + `Decision`. Phase 3 added the Cost Engine
//! (3-tier budget). Phase 4.6 added the secret-redaction barrier
//! (`redact::Vault` + `redact::Redactor`) — see PRD §13.4.

pub mod cost;
pub mod redact;

pub use cost::{estimate_usd, BudgetCfg, BudgetCheck, BudgetLevel, CostEngine, CostEvent, CostSummary};
pub use redact::{
    classify_secret, default_vault_path, fingerprint_of,
    Redactor, SecretKind, Vault, VaultEntry,
};

use serde::{Deserialize, Serialize};

/// Permission ladder, mirrors PRD §13.1 exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Permission {
    P0, // read-only
    P1, // workspace write (default)
    P2, // local-safe shell
    P3, // network
    P4, // browser control
    P5, // user-dir write
    P6, // system modification
    P7, // credential ops
    P8, // production ops
}

impl Permission {
    pub const DEFAULT: Self = Self::P1;
    /// Channel senders never exceed P4 (PRD §13.2 / §36.3).
    pub const CHANNEL_MAX: Self = Self::P4;

    pub fn allows(self, required: Permission) -> bool {
        self >= required
    }
}

/// Outcome of a policy evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Block { reason: String },
    Confirm { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_ordering() {
        assert!(Permission::P3 > Permission::P1);
        assert!(Permission::P1.allows(Permission::P0));
        assert!(!Permission::P0.allows(Permission::P5));
    }

    #[test]
    fn channel_cap_below_full_admin() {
        assert!(Permission::CHANNEL_MAX < Permission::P8);
    }

    #[test]
    fn default_is_workspace_write() {
        assert_eq!(Permission::DEFAULT, Permission::P1);
    }
}

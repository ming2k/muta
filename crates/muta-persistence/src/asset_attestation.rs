//! Universal Asset Attestation Ledger (ADR-0243): Cryptographic fingerprinting
//! and trust persistence for all external capabilities.

use std::time::{SystemTime, UNIX_EPOCH};
use muta_contracts::security::{AssetSpec, AttestationStatus};
use serde::{Deserialize, Serialize};
use crate::db::PersistenceHandle;

/// Persisted record of an asset attestation grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetAttestationRecord {
    pub fingerprint: String,
    pub spec: AssetSpec,
    pub status: AttestationStatus,
    pub created_at_s: u64,
    pub updated_at_s: u64,
}

/// Durable store for universal asset attestation decisions (ADR-0243).
#[derive(Debug, Clone)]
pub struct AssetAttestationLedger {
    handle: PersistenceHandle,
}

impl AssetAttestationLedger {
    pub fn load() -> Self {
        Self {
            handle: crate::db::get_persistence_handle(),
        }
    }

    pub fn for_handle(handle: PersistenceHandle) -> Self {
        Self { handle }
    }

    /// Check the attestation status for a given asset specification.
    pub fn status(&self, spec: &AssetSpec) -> AttestationStatus {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:{fingerprint}");
        let reader = match self.handle.reader() {
            Ok(r) => r,
            Err(_) => return AttestationStatus::Quarantined,
        };
        match reader.get_json::<AssetAttestationRecord>(&key) {
            Ok(Some(record)) => record.status,
            _ => AttestationStatus::Quarantined,
        }
    }

    /// Whether this asset is attested as trusted (or session-ephemeral).
    pub fn is_trusted(&self, spec: &AssetSpec) -> bool {
        self.status(spec).is_trusted()
    }

    /// Explicitly trust and record the fingerprint for an asset (e.g. from CLI or prompt approval).
    pub fn trust_asset(&self, spec: &AssetSpec) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:{fingerprint}");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = AssetAttestationRecord {
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Trusted,
            created_at_s: now,
            updated_at_s: now,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot persist asset attestation: {e}"))
    }

    /// Explicitly quarantine an asset.
    pub fn quarantine_asset(&self, spec: &AssetSpec) -> Result<(), String> {
        let fingerprint = spec.fingerprint();
        let key = format!("attestation:{fingerprint}");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = AssetAttestationRecord {
            fingerprint,
            spec: spec.clone(),
            status: AttestationStatus::Quarantined,
            created_at_s: now,
            updated_at_s: now,
        };

        self.handle
            .set_json_blocking(&key, &record)
            .map_err(|e| format!("cannot quarantine asset: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn attestation_ledger_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_assets.db");
        let handle = PersistenceHandle::spawn(db_path, None);
        let ledger = AssetAttestationLedger::for_handle(handle);

        let mut env = BTreeMap::new();
        env.insert("PYTHONPATH".into(), "/opt/lib".into());
        let spec = AssetSpec::Process {
            command: vec!["python3".into(), "-m".into(), "philpapers_mcp".into()],
            env,
        };

        // Initially untrusted / quarantined
        assert_eq!(ledger.status(&spec), AttestationStatus::Quarantined);
        assert!(!ledger.is_trusted(&spec));

        // Trust asset
        ledger.trust_asset(&spec).unwrap();
        assert_eq!(ledger.status(&spec), AttestationStatus::Trusted);
        assert!(ledger.is_trusted(&spec));

        // Quarantine asset
        ledger.quarantine_asset(&spec).unwrap();
        assert_eq!(ledger.status(&spec), AttestationStatus::Quarantined);
        assert!(!ledger.is_trusted(&spec));
    }
}

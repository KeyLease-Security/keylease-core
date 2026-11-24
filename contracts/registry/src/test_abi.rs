extern crate std;

use std::format;
use std::fs;
use std::vec;
use std::vec::Vec;

use stellar_xdr::curr::{Limits, ReadXdr, ScSpecEntry};

fn collect_spec_entries() -> Vec<ScSpecEntry> {
    let mut entries: Vec<ScSpecEntry> = Vec::new();

    // Contract functions (exported entry points)
    let fns: Vec<Vec<u8>> = vec![
        crate::KeyLeaseRegistry::spec_xdr_init().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_set_token().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_propose_upgrade().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_cancel_upgrade().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_execute_upgrade().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_register_service().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_set_service_active().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_update_service().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_set_service_attestor().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_create_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_settle_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_settle_lease_attested().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_revoke_expired_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_bump_service().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_bump_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_get_admin().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_get_token().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_get_pending_upgrade().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_version().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_service_count().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_lease_count().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_get_service().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_services_for_provider().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_get_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_quote_lease().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_lease_status().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_remaining_calls().to_vec(),
        crate::KeyLeaseRegistry::spec_xdr_locked_balance().to_vec(),
    ];
    for xdr in fns {
        entries.push(ScSpecEntry::from_xdr(xdr, Limits::none()).unwrap());
    }

    // Events
    let events: Vec<Vec<u8>> = vec![
        crate::events::RegistryInitialized::spec_xdr().to_vec(),
        crate::events::EscrowTokenSet::spec_xdr().to_vec(),
        crate::events::ServiceRegistered::spec_xdr().to_vec(),
        crate::events::ServiceStatusChanged::spec_xdr().to_vec(),
        crate::events::ServiceRateUpdated::spec_xdr().to_vec(),
        crate::events::ServiceAttestorSet::spec_xdr().to_vec(),
        crate::events::LeaseCreated::spec_xdr().to_vec(),
        crate::events::LeaseSettled::spec_xdr().to_vec(),
        crate::events::LeaseRevoked::spec_xdr().to_vec(),
        crate::events::UpgradeProposed::spec_xdr().to_vec(),
        crate::events::UpgradeCancelled::spec_xdr().to_vec(),
        crate::events::ContractUpgraded::spec_xdr().to_vec(),
    ];
    for xdr in events {
        entries.push(ScSpecEntry::from_xdr(xdr, Limits::none()).unwrap());
    }

    // Errors
    entries.push(ScSpecEntry::from_xdr(crate::Error::spec_xdr(), Limits::none()).unwrap());

    entries
}

#[test]
fn abi_snapshot() {
    let entries = collect_spec_entries();
    let json = serde_json::to_string_pretty(&entries).unwrap();
    let snapshot_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("abi_snapshot.json");
    if std::env::var("UPDATE_ABI_SNAPSHOT").as_deref() == Ok("1") {
        fs::write(&snapshot_path, format!("{json}\n")).unwrap();
        return;
    }
    let expected = fs::read_to_string(&snapshot_path)
        .unwrap_or_else(|_| panic!("missing ABI snapshot at {}; run UPDATE_ABI_SNAPSHOT=1 cargo test -p keylease-registry abi_snapshot -- --nocapture", snapshot_path.display()));
    let expected_entries: Vec<ScSpecEntry> = serde_json::from_str(&expected).unwrap();
    assert_eq!(entries, expected_entries, "ABI snapshot mismatch; if this change is intentional, update with UPDATE_ABI_SNAPSHOT=1 cargo test -p keylease-registry abi_snapshot -- --nocapture");
}

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use stellar_xdr::curr::{Limits, ReadXdr, ScAddress, ScMap, ScMapEntry, ScVal};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseRecord {
    pub lease_id: u64,
    pub service_id: u64,
    pub consumer: String,
    pub allocated_calls: u32,
    pub used_calls: u32,
    pub expires_at: u64,
    pub locked_deposit: i128,
    pub settled: bool,
    pub provider: Option<String>,
    pub payout: Option<i128>,
    pub refund: Option<i128>,
    pub created_ledger: u32,
    pub settled_ledger: Option<u32>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseLedger {
    pub leases: BTreeMap<u64, LeaseRecord>,
    pub last_indexed_ledger: u32,
}

impl LeaseLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_or_create<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let data = fs::read_to_string(path)?;
            serde_json::from_str(&data)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        } else {
            Ok(Self::new())
        }
    }

    pub fn save<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut file = File::create(path)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    pub fn apply_event(&mut self, event: &ParsedEvent, ledger: u32) {
        if ledger > self.last_indexed_ledger {
            self.last_indexed_ledger = ledger;
        }

        match event {
            ParsedEvent::LeaseCreated {
                lease_id,
                service_id,
                consumer,
                allocated_calls,
                expires_at,
                locked_deposit,
            } => {
                let record = LeaseRecord {
                    lease_id: *lease_id,
                    service_id: *service_id,
                    consumer: consumer.clone(),
                    allocated_calls: *allocated_calls,
                    used_calls: 0,
                    expires_at: *expires_at,
                    locked_deposit: *locked_deposit,
                    settled: false,
                    provider: None,
                    payout: None,
                    refund: None,
                    created_ledger: ledger,
                    settled_ledger: None,
                };
                println!(
                    "[LEASE_CREATED] Lease #{}: service={} consumer={} calls={} deposit={} stroops expires_at={}",
                    record.lease_id,
                    record.service_id,
                    record.consumer,
                    record.allocated_calls,
                    record.locked_deposit,
                    record.expires_at
                );
                self.leases.insert(*lease_id, record);
            }
            ParsedEvent::LeaseSettled {
                lease_id,
                provider,
                verified_calls,
                payout,
                refund,
            } => {
                let entry = self.leases.entry(*lease_id).or_insert_with(|| LeaseRecord {
                    lease_id: *lease_id,
                    service_id: 0,
                    consumer: String::from("unknown"),
                    allocated_calls: *verified_calls,
                    used_calls: 0,
                    expires_at: 0,
                    locked_deposit: *payout + *refund,
                    settled: false,
                    provider: None,
                    payout: None,
                    refund: None,
                    created_ledger: ledger,
                    settled_ledger: None,
                });
                entry.settled = true;
                entry.used_calls = *verified_calls;
                entry.provider = Some(provider.clone());
                entry.payout = Some(*payout);
                entry.refund = Some(*refund);
                entry.settled_ledger = Some(ledger);

                println!(
                    "[LEASE_SETTLED] Lease #{}: provider={} verified_calls={} payout={} stroops refund={} stroops (consumer={})",
                    entry.lease_id,
                    provider,
                    verified_calls,
                    payout,
                    refund,
                    entry.consumer
                );
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedEvent {
    LeaseCreated {
        lease_id: u64,
        service_id: u64,
        consumer: String,
        allocated_calls: u32,
        expires_at: u64,
        locked_deposit: i128,
    },
    LeaseSettled {
        lease_id: u64,
        provider: String,
        verified_calls: u32,
        payout: i128,
        refund: i128,
    },
}

#[derive(Debug, Deserialize)]
pub struct RawRpcEvent {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub ledger: Option<u32>,
    #[serde(rename = "ledgerClosedAt")]
    pub ledger_closed_at: Option<String>,
    #[serde(rename = "contractId")]
    pub contract_id: Option<String>,
    pub topic: Vec<String>,
    pub value: serde_json::Value,
}

pub fn parse_scval_symbol(val: &ScVal) -> Option<String> {
    match val {
        ScVal::Symbol(sym) => Some(sym.to_string()),
        _ => None,
    }
}

pub fn parse_scval_u64(val: &ScVal) -> Option<u64> {
    match val {
        ScVal::U64(v) => Some(*v),
        _ => None,
    }
}

pub fn parse_scval_u32(val: &ScVal) -> Option<u32> {
    match val {
        ScVal::U32(v) => Some(*v),
        _ => None,
    }
}

pub fn parse_scval_i128(val: &ScVal) -> Option<i128> {
    match val {
        ScVal::I128(parts) => {
            let hi = parts.hi as i128;
            let lo = parts.lo as u128;
            Some((hi << 64) | (lo as i128))
        }
        _ => None,
    }
}

pub fn parse_scval_address(val: &ScVal) -> Option<String> {
    match val {
        ScVal::Address(addr) => match addr {
            ScAddress::Account(account_id) => {
                let stellar_xdr::curr::AccountId(
                    stellar_xdr::curr::PublicKey::PublicKeyTypeEd25519(stellar_xdr::curr::Uint256(
                        bytes,
                    )),
                ) = account_id;
                Some(
                    stellar_strkey::ed25519::PublicKey(*bytes)
                        .to_string()
                        .as_str()
                        .to_string(),
                )
            }
            ScAddress::Contract(contract_hash) => Some(
                stellar_strkey::Contract(contract_hash.0.clone().into())
                    .to_string()
                    .as_str()
                    .to_string(),
            ),
            _ => None,
        },
        _ => None,
    }
}

fn get_map_entry<'a>(entries: &'a [ScMapEntry], key: &str) -> Option<&'a ScVal> {
    for entry in entries {
        if let ScVal::Symbol(ref sym) = entry.key {
            if sym.to_string() == key {
                return Some(&entry.val);
            }
        }
    }
    None
}

pub fn parse_event(raw: &RawRpcEvent) -> Option<ParsedEvent> {
    if raw.topic.len() < 2 {
        return None;
    }

    let topic0 = ScVal::from_xdr_base64(&raw.topic[0], Limits::none()).ok()?;
    let topic1 = ScVal::from_xdr_base64(&raw.topic[1], Limits::none()).ok()?;

    let event_name = parse_scval_symbol(&topic0)?;
    let lease_id = parse_scval_u64(&topic1)?;

    let value_val = match &raw.value {
        serde_json::Value::String(b64) => ScVal::from_xdr_base64(b64, Limits::none()).ok()?,
        _ => return None,
    };

    let entries = match value_val {
        ScVal::Map(Some(ScMap(entries))) => entries,
        _ => return None,
    };

    match event_name.as_str() {
        "lease_created" => {
            let service_id = parse_scval_u64(get_map_entry(&entries, "service_id")?)?;
            let consumer = parse_scval_address(get_map_entry(&entries, "consumer")?)?;
            let allocated_calls = parse_scval_u32(get_map_entry(&entries, "allocated_calls")?)?;
            let expires_at = parse_scval_u64(get_map_entry(&entries, "expires_at")?)?;
            let locked_deposit = parse_scval_i128(get_map_entry(&entries, "locked_deposit")?)?;

            Some(ParsedEvent::LeaseCreated {
                lease_id,
                service_id,
                consumer,
                allocated_calls,
                expires_at,
                locked_deposit,
            })
        }
        "lease_settled" => {
            let provider = parse_scval_address(get_map_entry(&entries, "provider")?)?;
            let verified_calls = parse_scval_u32(get_map_entry(&entries, "verified_calls")?)?;
            let payout = parse_scval_i128(get_map_entry(&entries, "payout")?)?;
            let refund = parse_scval_i128(get_map_entry(&entries, "refund")?)?;

            Some(ParsedEvent::LeaseSettled {
                lease_id,
                provider,
                verified_calls,
                payout,
                refund,
            })
        }
        _ => None,
    }
}

fn print_usage() {
    eprintln!(
        r#"KeyLease Event Indexer

USAGE:
    keylease-indexer [OPTIONS]

OPTIONS:
    --rpc-url <URL>           Soroban RPC URL [default: https://soroban-testnet.stellar.org]
    --contract-id <ID>        KeyLeaseRegistry contract ID on Stellar
    --start-ledger <N>        Starting ledger sequence number [default: 0]
    --ledger-file <PATH>      File path to persist the lease ledger [default: lease_ledger.json]
    --fixture <FILE>          Read events from a local JSON fixture file (offline mode)
    --once                    Poll once and exit instead of continuous loop
    --poll-interval <SECS>    Interval in seconds between RPC polls [default: 5]
    -h, --help                Print this help information
"#
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mut rpc_url = std::env::var("KEYLEASE_RPC_URL")
        .unwrap_or_else(|_| "https://soroban-testnet.stellar.org".to_string());
    let mut contract_id = std::env::var("KEYLEASE_CONTRACT_ID")
        .unwrap_or_else(|_| "CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62".to_string());
    let mut ledger_file =
        std::env::var("KEYLEASE_LEDGER_PATH").unwrap_or_else(|_| "lease_ledger.json".to_string());
    let mut start_ledger: u32 = 0;
    let mut fixture_file: Option<PathBuf> = None;
    let mut once = false;
    let mut poll_interval_secs: u64 = 5;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rpc-url" if i + 1 < args.len() => {
                rpc_url = args[i + 1].clone();
                i += 2;
            }
            "--contract-id" if i + 1 < args.len() => {
                contract_id = args[i + 1].clone();
                i += 2;
            }
            "--start-ledger" if i + 1 < args.len() => {
                start_ledger = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--ledger-file" if i + 1 < args.len() => {
                ledger_file = args[i + 1].clone();
                i += 2;
            }
            "--fixture" if i + 1 < args.len() => {
                fixture_file = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--once" => {
                once = true;
                i += 1;
            }
            "--poll-interval" if i + 1 < args.len() => {
                poll_interval_secs = args[i + 1].parse().unwrap_or(5);
                i += 2;
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
    }

    let mut ledger = LeaseLedger::load_or_create(&ledger_file)?;
    if start_ledger == 0 && ledger.last_indexed_ledger > 0 {
        start_ledger = ledger.last_indexed_ledger + 1;
    }

    if let Some(fixture) = fixture_file {
        println!("Reading events from fixture: {}", fixture.display());
        let content = fs::read_to_string(&fixture)?;
        let events: Vec<RawRpcEvent> = serde_json::from_str(&content)?;
        for event in events {
            let seq = event.ledger.unwrap_or(0);
            if let Some(parsed) = parse_event(&event) {
                ledger.apply_event(&parsed, seq);
            }
        }
        ledger.save(&ledger_file)?;
        println!(
            "Indexed {} active leases into {}",
            ledger.leases.len(),
            ledger_file
        );
        return Ok(());
    }

    println!("Starting KeyLease event indexer (read-only mode, no secret keys required)");
    println!("RPC URL:      {rpc_url}");
    println!("Contract ID:  {contract_id}");
    println!("Start ledger: {start_ledger}");
    println!("Ledger store: {ledger_file}");

    let mut current_ledger = start_ledger;

    loop {
        let request_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getEvents",
            "params": {
                "startLedger": current_ledger,
                "filters": [
                    {
                        "type": "contract",
                        "contractIds": [contract_id]
                    }
                ],
                "limit": 100
            }
        });

        match ureq::post(&rpc_url).send_json(&request_payload) {
            Ok(response) => {
                let resp_val: serde_json::Value = response.into_json()?;
                if let Some(events_arr) = resp_val["result"]["events"].as_array() {
                    for ev in events_arr {
                        if let Ok(raw_ev) = serde_json::from_value::<RawRpcEvent>(ev.clone()) {
                            let seq = raw_ev.ledger.unwrap_or(current_ledger);
                            if let Some(parsed) = parse_event(&raw_ev) {
                                ledger.apply_event(&parsed, seq);
                            }
                            if seq >= current_ledger {
                                current_ledger = seq + 1;
                            }
                        }
                    }
                }
                if let Some(latest) = resp_val["result"]["latestLedger"].as_u64() {
                    let latest = latest as u32;
                    if current_ledger <= latest {
                        current_ledger = latest + 1;
                    }
                }
                ledger.save(&ledger_file)?;
            }
            Err(e) => {
                eprintln!("[WARN] RPC request failed: {e}; will retry");
            }
        }

        if once {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(poll_interval_secs));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_lease_created_and_settled_fixtures() {
        use stellar_xdr::curr::{ScSymbol, WriteXdr};

        let topic_created_0 = ScVal::Symbol(ScSymbol("lease_created".try_into().unwrap()))
            .to_xdr_base64(Limits::none())
            .unwrap();
        let topic_lease_id = ScVal::U64(42).to_xdr_base64(Limits::none()).unwrap();

        let raw_created = RawRpcEvent {
            id: Some("0000000000000001".into()),
            event_type: Some("contract".into()),
            ledger: Some(100),
            ledger_closed_at: Some("2026-01-01T00:00:00Z".into()),
            contract_id: Some("CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62".into()),
            topic: vec![topic_created_0, topic_lease_id.clone()],
            value: serde_json::Value::String(create_lease_created_val_xdr()),
        };

        let parsed = parse_event(&raw_created).expect("Should parse lease_created");
        match parsed {
            ParsedEvent::LeaseCreated {
                lease_id,
                service_id,
                consumer,
                allocated_calls,
                expires_at,
                locked_deposit,
            } => {
                assert_eq!(lease_id, 42);
                assert_eq!(service_id, 1);
                assert!(!consumer.is_empty());
                assert_eq!(allocated_calls, 10);
                assert_eq!(expires_at, 5000);
                assert_eq!(locked_deposit, 1000);
            }
            _ => panic!("Expected LeaseCreated variant"),
        }

        let topic_settled_0 = ScVal::Symbol(ScSymbol("lease_settled".try_into().unwrap()))
            .to_xdr_base64(Limits::none())
            .unwrap();

        let raw_settled = RawRpcEvent {
            id: Some("0000000000000002".into()),
            event_type: Some("contract".into()),
            ledger: Some(120),
            ledger_closed_at: Some("2026-01-01T00:01:00Z".into()),
            contract_id: Some("CCEZYHUJUICXVPLH6UC2OFZL2RG23QQ7EMZOWE32WVQPT25DCTIAFH62".into()),
            topic: vec![topic_settled_0, topic_lease_id],
            value: serde_json::Value::String(create_lease_settled_val_xdr()),
        };

        let parsed = parse_event(&raw_settled).expect("Should parse lease_settled");
        match parsed {
            ParsedEvent::LeaseSettled {
                lease_id,
                provider,
                verified_calls,
                payout,
                refund,
            } => {
                assert_eq!(lease_id, 42);
                assert!(!provider.is_empty());
                assert_eq!(verified_calls, 6);
                assert_eq!(payout, 600);
                assert_eq!(refund, 400);
            }
            _ => panic!("Expected LeaseSettled variant"),
        }
    }

    #[test]
    fn test_unrelated_event_filtered() {
        use stellar_xdr::curr::{ScSymbol, WriteXdr};

        let topic_other = ScVal::Symbol(ScSymbol("other_event".try_into().unwrap()))
            .to_xdr_base64(Limits::none())
            .unwrap();
        let topic_id = ScVal::U64(42).to_xdr_base64(Limits::none()).unwrap();

        let raw_other = RawRpcEvent {
            id: Some("0000000000000003".into()),
            event_type: Some("contract".into()),
            ledger: Some(100),
            ledger_closed_at: None,
            contract_id: None,
            topic: vec![topic_other, topic_id],
            value: serde_json::Value::String(ScVal::Void.to_xdr_base64(Limits::none()).unwrap()),
        };

        assert!(parse_event(&raw_other).is_none());
    }

    #[test]
    fn test_malformed_or_missing_fields_rejected() {
        use stellar_xdr::curr::{ScSymbol, WriteXdr};

        let topic_created = ScVal::Symbol(ScSymbol("lease_created".try_into().unwrap()))
            .to_xdr_base64(Limits::none())
            .unwrap();

        // Less than 2 topics
        let raw_short_topic = RawRpcEvent {
            id: None,
            event_type: None,
            ledger: None,
            ledger_closed_at: None,
            contract_id: None,
            topic: vec![topic_created.clone()],
            value: serde_json::Value::String(create_lease_created_val_xdr()),
        };
        assert!(parse_event(&raw_short_topic).is_none());

        // Value is not a map
        let topic_id = ScVal::U64(1).to_xdr_base64(Limits::none()).unwrap();
        let raw_invalid_val = RawRpcEvent {
            id: None,
            event_type: None,
            ledger: None,
            ledger_closed_at: None,
            contract_id: None,
            topic: vec![topic_created, topic_id],
            value: serde_json::Value::String(
                ScVal::U32(123).to_xdr_base64(Limits::none()).unwrap(),
            ),
        };
        assert!(parse_event(&raw_invalid_val).is_none());
    }

    #[test]
    fn test_ledger_reconciliation_and_persistence() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "keylease_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ledger_path = tmp_dir.join("ledger.json");

        let mut ledger = LeaseLedger::new();
        let created_ev = ParsedEvent::LeaseCreated {
            lease_id: 1,
            service_id: 2,
            consumer: "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF".into(),
            allocated_calls: 100,
            expires_at: 10_000,
            locked_deposit: 5_000,
        };

        ledger.apply_event(&created_ev, 1000);
        assert_eq!(ledger.leases.len(), 1);
        let rec = ledger.leases.get(&1).unwrap();
        assert_eq!(rec.lease_id, 1);
        assert_eq!(rec.allocated_calls, 100);
        assert_eq!(rec.locked_deposit, 5_000);
        assert!(!rec.settled);

        let settled_ev = ParsedEvent::LeaseSettled {
            lease_id: 1,
            provider: "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBHP4".into(),
            verified_calls: 80,
            payout: 4_000,
            refund: 1_000,
        };

        ledger.apply_event(&settled_ev, 1010);
        let settled_rec = ledger.leases.get(&1).unwrap();
        assert!(settled_rec.settled);
        assert_eq!(settled_rec.used_calls, 80);
        assert_eq!(settled_rec.payout, Some(4_000));
        assert_eq!(settled_rec.refund, Some(1_000));
        assert_eq!(settled_rec.settled_ledger, Some(1010));

        ledger.save(&ledger_path).expect("Failed to save ledger");
        let loaded = LeaseLedger::load_or_create(&ledger_path).expect("Failed to load ledger");
        assert_eq!(loaded, ledger);

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    fn create_lease_created_val_xdr() -> String {
        use stellar_xdr::curr::{Int128Parts, ScMap, ScMapEntry, ScSymbol, ScVal, WriteXdr};
        let entries = vec![
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("allocated_calls".try_into().unwrap())),
                val: ScVal::U32(10),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("consumer".try_into().unwrap())),
                val: ScVal::Address(ScAddress::Account(stellar_xdr::curr::AccountId(
                    stellar_xdr::curr::PublicKey::PublicKeyTypeEd25519(stellar_xdr::curr::Uint256(
                        [1u8; 32],
                    )),
                ))),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("expires_at".try_into().unwrap())),
                val: ScVal::U64(5000),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("locked_deposit".try_into().unwrap())),
                val: ScVal::I128(Int128Parts { hi: 0, lo: 1000 }),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("service_id".try_into().unwrap())),
                val: ScVal::U64(1),
            },
        ];
        let val = ScVal::Map(Some(ScMap(entries.try_into().unwrap())));
        val.to_xdr_base64(Limits::none()).unwrap()
    }

    fn create_lease_settled_val_xdr() -> String {
        use stellar_xdr::curr::{Int128Parts, ScMap, ScMapEntry, ScSymbol, ScVal, WriteXdr};
        let entries = vec![
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("payout".try_into().unwrap())),
                val: ScVal::I128(Int128Parts { hi: 0, lo: 600 }),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("provider".try_into().unwrap())),
                val: ScVal::Address(ScAddress::Account(stellar_xdr::curr::AccountId(
                    stellar_xdr::curr::PublicKey::PublicKeyTypeEd25519(stellar_xdr::curr::Uint256(
                        [2u8; 32],
                    )),
                ))),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("refund".try_into().unwrap())),
                val: ScVal::I128(Int128Parts { hi: 0, lo: 400 }),
            },
            ScMapEntry {
                key: ScVal::Symbol(ScSymbol("verified_calls".try_into().unwrap())),
                val: ScVal::U32(6),
            },
        ];
        let val = ScVal::Map(Some(ScMap(entries.try_into().unwrap())));
        val.to_xdr_base64(Limits::none()).unwrap()
    }
}

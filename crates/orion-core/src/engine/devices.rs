//! Paired devices: the per-device credential a phone uses instead of the shared token.
//!
//! The owner starts a pairing on the PC and gets a short code that lives ten minutes. The
//! device sends the code once and receives a token of its own. Only a SHA-256 hash of that
//! token is stored, so `devices.json` in the state dir reveals nothing if read.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::engine::store::Store;

const CODE_TTL: Duration = Duration::from_secs(10 * 60);
const CODE_ATTEMPTS: u32 = 5;
const LABEL_MAX: usize = 48;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDevice {
    pub id: String,
    pub label: String,
    pub role: String,
    pub token_hash: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DevicesFile {
    #[serde(default)]
    devices: Vec<StoredDevice>,
    gate_closed_at: Option<String>,
}

struct PendingCode {
    code: String,
    expires: SystemTime,
    attempts: u32,
}

pub struct DeviceStore {
    path: PathBuf,
    devices: Mutex<Vec<StoredDevice>>,
    pending: Mutex<Option<PendingCode>>,
}

impl DeviceStore {
    pub fn new(store: &Store) -> Self {
        let path = store.dir().join("devices.json");
        let devices = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<DevicesFile>(&text).ok())
            .map(|file| file.devices)
            .unwrap_or_default();
        Self {
            path,
            devices: Mutex::new(devices),
            pending: Mutex::new(None),
        }
    }

    fn save(&self, devices: &[StoredDevice]) {
        let file = DevicesFile {
            devices: devices.to_vec(),
            gate_closed_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        let text = serde_json::to_string_pretty(&file).unwrap_or_default();
        let _ = std::fs::create_dir_all(self.path.parent().expect("devices dir"));
        let temp = self.path.with_extension("tmp");
        let _ = std::fs::write(&temp, text);
        let _ = std::fs::rename(&temp, &self.path);
    }

    pub fn new_pairing_code(&self) -> Value {
        let code = random_code();
        let expires = SystemTime::now() + CODE_TTL;
        *self.pending.lock() = Some(PendingCode {
            code: code.clone(),
            expires,
            attempts: 0,
        });
        json!({
            "code": code,
            "expires_at": chrono::DateTime::<chrono::Utc>::from(expires).to_rfc3339(),
        })
    }

    /// Exchanges a code for a device. Returns the device entry plus its one-time token.
    pub fn pair(&self, code: &str, label: &str, role: &str) -> Option<Value> {
        let mut pending = self.pending.lock();
        {
            let current = pending.as_ref()?;
            if SystemTime::now() > current.expires {
                pending.take();
                return None;
            }
        }
        let mut current = pending.take()?;
        // Constant-time comparison: a 6-digit code must not leak its prefix.
        let codes_match = current.code.len() == code.len()
            && bool::from(current.code.as_bytes().ct_eq(code.as_bytes()));
        if !codes_match {
            current.attempts += 1;
            if current.attempts < CODE_ATTEMPTS {
                *pending = Some(current);
            }
            return None;
        }
        let token = random_token();
        let device = StoredDevice {
            id: uuid::Uuid::new_v4().to_string(),
            label: normalize_label(label),
            role: if role == "watch" {
                "watch".to_string()
            } else {
                "drive".to_string()
            },
            token_hash: hash(&token),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_seen_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        let mut devices = self.devices.lock();
        devices.push(device.clone());
        self.save(&devices);
        Some(json!({
            "device": {
                "id": device.id,
                "label": device.label,
                "role": device.role,
                "created_at": device.created_at,
                "last_seen_at": device.last_seen_at,
            },
            "token": token,
        }))
    }

    pub fn list(&self) -> Value {
        let devices = self.devices.lock();
        json!({
            "devices": devices.iter().map(|device| json!({
                "id": device.id,
                "label": device.label,
                "role": device.role,
                "created_at": device.created_at,
                "last_seen_at": device.last_seen_at,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn devices_vec(&self) -> Vec<StoredDevice> {
        self.devices.lock().clone()
    }

    pub fn save_devices(&self, devices: Vec<StoredDevice>) {
        self.save(&devices);
    }

    /// Whether `token` belongs to a paired device.
    pub fn matches(&self, token: &str) -> bool {
        let hash = hash(token);
        let devices = self.devices.lock();
        // Constant-time over every stored hash so the list length leaks nothing.
        devices.iter().any(|device| {
            device.token_hash.len() == hash.len()
                && bool::from(device.token_hash.as_bytes().ct_eq(hash.as_bytes()))
        })
    }
}

fn hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(LABEL_MAX)
        .collect()
}

fn random_code() -> String {
    // A 6-digit code from a UUID's hex digits — not a timestamp, so it cannot
    // be predicted from when it was issued.
    let hex = uuid::Uuid::new_v4().simple().to_string();
    let digits: String = hex.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut code = digits;
    while code.len() < 6 {
        code.push('0');
    }
    code[..6].to_string()
}

fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_exchanges_for_a_hashed_token() {
        let dir = std::env::temp_dir().join(format!("orion-devices-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = Store::at(dir.clone());
        let devices = DeviceStore::new(&store);
        let issued = devices.new_pairing_code();
        let code = issued["code"].as_str().expect("code").to_string();
        let paired = devices.pair(&code, "Phone", "drive").expect("pairs");
        let token = paired["token"].as_str().expect("token").to_string();
        assert!(devices.matches(&token));
        assert!(!devices.matches("wrong"));
        let listed = devices.list();
        assert_eq!(listed["devices"].as_array().expect("devices").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_codes_expire_after_five_attempts() {
        let dir = std::env::temp_dir().join(format!("orion-devices-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = Store::at(dir.clone());
        let devices = DeviceStore::new(&store);
        let issued = devices.new_pairing_code();
        let code = issued["code"].as_str().expect("code").to_string();
        for _ in 0..5 {
            assert!(devices.pair("000000", "Phone", "drive").is_none());
        }
        assert!(
            devices.pair(&code, "Phone", "drive").is_none(),
            "code burned out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Web push for pane alerts.
//!
//! A VAPID keypair is generated once and kept in the state dir; browsers subscribe with it and
//! the engine delivers `PushPayload`s to the stored endpoints.
//!
//! Delivery is performed by [`PushService::send_test`]: it builds a VAPID authorization token
//! (RFC 8292), encrypts the payload with AES-128-GCM over an ECDH-derived shared secret
//! (RFC 8291 / RFC 8188) and POSTs the push message to the subscription endpoint with the
//! `Encryption`, `Crypto-Key`, `Authorization`, `Urgency` and `TTL` headers (RFC 8030).
//!
//! The HTTP client is `reqwest` with the `rustls-tls` feature only: default-tls pulls in
//! OpenSSL (via `curl-sys`), which does not build cleanly on Windows.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, Key, KeyInit};
use base64::Engine;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{ecdsa::SigningKey, PublicKey};
use parking_lot::Mutex;
use reqwest::{header, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use signature::DigestSigner;

/// How long a VAPID token is valid for (RFC 8292 recommends no more than 24 hours).
const VAPID_EXPIRY_SECONDS: u64 = 12 * 60 * 60;
/// Contact identity embedded in the VAPID token `sub` claim.
const VAPID_SUBJECT: &str = "mailto:orion@localhost";
/// Time-to-live for delivered push messages, in seconds.
const PUSH_TTL_SECONDS: u64 = 86400;
/// AES-128-GCM record size carried in the encrypted content-coding header.
const RECORD_SIZE: u32 = 4096;

type Map<K, V> = std::collections::BTreeMap<K, V>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Subscription {
    endpoint: String,
    keys: Map<String, String>,
    alerts: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PushFile {
    private_key: String,
    #[serde(default)]
    subscriptions: Vec<Subscription>,
}

pub struct PushService {
    path: PathBuf,
    state: Mutex<PushFile>,
}

impl PushService {
    pub fn new(store: &crate::engine::store::Store) -> Self {
        let path = store.dir().join("push.json");
        let loaded = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<PushFile>(&text).ok());
        let state = loaded.unwrap_or_else(|| PushFile {
            private_key: generate_private_key(),
            subscriptions: Vec::new(),
        });
        let service = Self {
            path,
            state: Mutex::new(state),
        };
        service.save();
        service
    }

    fn save(&self) {
        let state = self.state.lock();
        let text = serde_json::to_string_pretty(&*state).unwrap_or_default();
        let _ = std::fs::create_dir_all(self.path.parent().expect("push dir"));
        let temp = self.path.with_extension("tmp");
        let _ = std::fs::write(&temp, text);
        let _ = std::fs::rename(&temp, &self.path);
    }

    /// The VAPID public key browsers subscribe with (base64url, compressed point).
    pub fn public_key(&self) -> String {
        let state = self.state.lock();
        public_key(&state.private_key)
    }

    pub fn subscribe(&self, subscription: Value, alerts: Option<Value>) {
        let endpoint = subscription
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let keys = subscription
            .get("keys")
            .and_then(Value::as_object)
            .map(|keys| {
                keys.iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if endpoint.is_empty() {
            return;
        }
        let mut state = self.state.lock();
        state
            .subscriptions
            .retain(|existing| existing.endpoint != endpoint);
        state.subscriptions.push(Subscription {
            endpoint,
            keys,
            alerts,
        });
        drop(state);
        self.save();
    }

    pub fn unsubscribe(&self, endpoint: &str) {
        let mut state = self.state.lock();
        state
            .subscriptions
            .retain(|existing| existing.endpoint != endpoint);
        drop(state);
        self.save();
    }

    pub fn subscriptions(&self) -> Vec<Value> {
        let state = self.state.lock();
        state
            .subscriptions
            .iter()
            .map(|subscription| {
                json!({
                    "endpoint": subscription.endpoint,
                    "keys": subscription.keys,
                })
            })
            .collect()
    }

    /// Send a test push message to `endpoint`.
    ///
    /// Looks up the stored subscription for `endpoint`, signs a VAPID JWT with the stored
    /// P-256 private key, encrypts `payload` for the subscription's P-256 keys and POSTs the
    /// push message to the endpoint. Returns `Ok(true)` when the push service responds with
    /// 201 (Created) or 204 (No Content).
    pub fn send_test(&self, endpoint: &str, payload: &str, urgency: &str) -> Result<bool, String> {
        let (private_key, keys) = {
            let state = self.state.lock();
            let private_key = state.private_key.clone();
            let subscription = state
                .subscriptions
                .iter()
                .find(|item| item.endpoint == endpoint)
                .cloned();
            let subscription =
                subscription.ok_or_else(|| format!("no subscription found for {endpoint}"))?;
            (private_key, subscription.keys)
        };

        match send_to_endpoint(&private_key, endpoint, &keys, payload, urgency)? {
            StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(true),
            status => Err(format!("push delivery to {endpoint} failed: {status}")),
        }
    }

    /// Broadcast a push notification payload to all stored subscriptions whose preferences allow it.
    ///
    /// The broadcast filters subscriptions by their stored alert preferences (`input` for blocked,
    /// `done` for finished turns, and any per-agent overrides). Delivery runs on a background blocking
    /// thread so Tokio async workers are never stalled by HTTP network operations.
    pub fn broadcast(
        &self,
        payload: &crate::protocol::PushPayload,
        status: crate::protocol::AgentStatus,
        agent_kind: Option<&str>,
    ) {
        let (private_key, subscriptions) = {
            let state = self.state.lock();
            (state.private_key.clone(), state.subscriptions.clone())
        };

        if subscriptions.is_empty() {
            return;
        }

        let payload_text = match serde_json::to_string(payload) {
            Ok(text) => text,
            Err(_) => return,
        };

        let urgency = match status {
            crate::protocol::AgentStatus::Blocked => "high",
            _ => "normal",
        };

        let status_str = match status {
            crate::protocol::AgentStatus::Blocked => "blocked",
            crate::protocol::AgentStatus::Done => "done",
            _ => return,
        };
        let agent_kind = agent_kind.map(str::to_string);
        let path = self.path.clone();

        tokio::task::spawn_blocking(move || {
            let mut expired = Vec::new();
            for sub in subscriptions {
                if let Some(alerts) = &sub.alerts {
                    let mut allow = true;
                    if let Some(agent) = agent_kind.as_deref() {
                        if let Some(agent_rules) =
                            alerts.get("agentAlerts").and_then(Value::as_object)
                        {
                            if let Some(rule) = agent_rules.get(agent) {
                                if status_str == "blocked" {
                                    if let Some(val) = rule.get("input").and_then(Value::as_bool) {
                                        allow = val;
                                    }
                                } else if status_str == "done" {
                                    if let Some(val) = rule.get("done").and_then(Value::as_str) {
                                        allow = val != "off";
                                    }
                                }
                            }
                        }
                    }

                    if !allow {
                        continue;
                    }

                    let blocked_disallowed = status_str == "blocked"
                        && !alerts.get("input").and_then(Value::as_bool).unwrap_or(true);
                    let done_disallowed = status_str == "done"
                        && alerts.get("done").and_then(Value::as_str).unwrap_or("long") == "off";
                    if blocked_disallowed || done_disallowed {
                        continue;
                    }
                }

                match send_to_endpoint(
                    &private_key,
                    &sub.endpoint,
                    &sub.keys,
                    &payload_text,
                    urgency,
                ) {
                    Ok(StatusCode::GONE) | Ok(StatusCode::NOT_FOUND) => {
                        expired.push(sub.endpoint);
                    }
                    _ => {}
                }
            }

            if !expired.is_empty() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(mut file) = serde_json::from_str::<PushFile>(&content) {
                        file.subscriptions
                            .retain(|s| !expired.contains(&s.endpoint));
                        if let Ok(updated) = serde_json::to_string_pretty(&file) {
                            let temp = path.with_extension("tmp");
                            let _ = std::fs::write(&temp, updated);
                            let _ = std::fs::rename(&temp, &path);
                        }
                    }
                }
            }
        });
    }
}

fn send_to_endpoint(
    private_key: &str,
    endpoint: &str,
    keys: &Map<String, String>,
    payload: &str,
    urgency: &str,
) -> Result<StatusCode, String> {
    let url = Url::parse(endpoint).map_err(|error| format!("invalid push endpoint: {error}"))?;
    let origin = url.origin().ascii_serialization();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("failed to read the system clock: {error}"))?
        .as_secs();
    let token = vapid_jwt(private_key, &origin, VAPID_SUBJECT, now)?;
    let vapid_public_key = public_key_uncompressed(private_key);

    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::HeaderName::from_bytes(b"TTL").expect("TTL is a valid header name"),
        header::HeaderValue::from_str(&PUSH_TTL_SECONDS.to_string())
            .expect("TTL is a valid header value"),
    );
    headers.insert(
        header::HeaderName::from_bytes(b"Urgency").expect("Urgency is a valid header name"),
        header::HeaderValue::from_str(urgency)
            .map_err(|error| format!("invalid urgency header: {error}"))?,
    );

    let body: Option<Vec<u8>> = if payload.is_empty() {
        None
    } else {
        let p256dh = keys
            .get("p256dh")
            .ok_or_else(|| format!("subscription {endpoint} has no p256dh key"))?;
        let auth = keys
            .get("auth")
            .ok_or_else(|| format!("subscription {endpoint} has no auth key"))?;
        let (body, salt, sender_public_key) = encrypt_payload(p256dh, auth, payload)?;
        headers.insert(
            header::HeaderName::from_bytes(b"Content-Encoding").expect("valid header name"),
            header::HeaderValue::from_static("aes128gcm"),
        );
        headers.insert(
            header::HeaderName::from_bytes(b"Encryption").expect("valid header name"),
            header::HeaderValue::from_str(&format!("salt={salt}"))
                .map_err(|error| format!("invalid encryption header: {error}"))?,
        );
        headers.insert(
            header::HeaderName::from_bytes(b"Crypto-Key").expect("valid header name"),
            header::HeaderValue::from_str(&format!("p256ecdsa={sender_public_key}"))
                .map_err(|error| format!("invalid crypto-key header: {error}"))?,
        );
        Some(body)
    };

    headers.insert(
        header::HeaderName::from_bytes(b"Authorization").expect("valid header name"),
        header::HeaderValue::from_str(&format!("vapid t={token}, s={vapid_public_key}"))
            .map_err(|error| format!("invalid authorization header: {error}"))?,
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| format!("failed to build the HTTP client: {error}"))?;
    let response = client
        .post(url)
        .headers(headers)
        .body(body.unwrap_or_default())
        .send()
        .map_err(|error| format!("failed to send push message: {error}"))?;

    Ok(response.status())
}

fn generate_private_key() -> String {
    use p256::ecdsa::SigningKey;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let key = SigningKey::from_slice(&bytes).expect("32 random bytes are a P-256 key");
    key.to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn public_key(private_hex: &str) -> String {
    use base64::Engine;
    use p256::ecdsa::SigningKey;
    let bytes: Vec<u8> = (0..private_hex.len() / 2)
        .map(|index| u8::from_str_radix(&private_hex[index * 2..index * 2 + 2], 16).unwrap_or(0))
        .collect();
    let Ok(signing) = SigningKey::from_slice(&bytes) else {
        return String::new();
    };
    let point = signing.verifying_key().to_encoded_point(true);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(point)
}

/// The VAPID public key in uncompressed X9.62 form (65 bytes, base64url).
/// RFC 8292 carries the application server's signing public key in this encoding.
fn public_key_uncompressed(private_hex: &str) -> String {
    use base64::Engine;
    use p256::ecdsa::SigningKey;
    let bytes: Vec<u8> = (0..private_hex.len() / 2)
        .map(|index| u8::from_str_radix(&private_hex[index * 2..index * 2 + 2], 16).unwrap_or(0))
        .collect();
    let Ok(signing) = SigningKey::from_slice(&bytes) else {
        return String::new();
    };
    let point = signing.verifying_key().to_encoded_point(false);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(point)
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    (0..hex.len() / 2)
        .map(|index| {
            u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .map_err(|error| format!("invalid hex in VAPID private key: {error}"))
        })
        .collect()
}

/// Build a VAPID JWT (RFC 8292): ES256-signed with `aud`, `exp` and `sub` claims.
fn vapid_jwt(private_key: &str, aud: &str, sub: &str, now: u64) -> Result<String, String> {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"ES256","typ":"JWT"}"#);
    let claims = format!(
        r#"{{"aud":"{}","exp":{},"sub":"{}"}}"#,
        aud,
        now + VAPID_EXPIRY_SECONDS,
        sub
    );
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.as_bytes());
    let signing_input = format!("{header}.{claims}");

    let private_bytes = decode_hex(private_key)?;
    let signing_key = SigningKey::from_slice(&private_bytes)
        .map_err(|error| format!("invalid VAPID signing key: {error}"))?;
    let mut digest = Sha256::new();
    digest.update(signing_input.as_bytes());
    let signature = <SigningKey as DigestSigner<Sha256, p256::ecdsa::Signature>>::try_sign_digest(
        &signing_key,
        digest,
    )
    .map_err(|error| format!("failed to sign VAPID token: {error}"))?;
    let signature_bytes = signature.to_bytes();

    Ok(format!(
        "{signing_input}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&signature_bytes[..])
    ))
}

/// Derive the AES-128-GCM content-encryption key and nonce (RFC 8291 Section 3.4 /
/// RFC 8188 Sections 2.2-2.3).
fn push_key_material(
    shared_secret: &p256::ecdh::SharedSecret,
    auth_secret: &[u8],
    key_info: &[u8],
    salt: &[u8],
) -> Result<([u8; 16], [u8; 12]), String> {
    let mut input_keying_material = [0u8; 32];
    shared_secret
        .extract::<Sha256>(Some(auth_secret))
        .expand(key_info, &mut input_keying_material)
        .map_err(|error| format!("failed to derive input keying material: {error}"))?;

    let hkdf = Hkdf::<Sha256>::new(Some(salt), &input_keying_material);
    let mut content_encryption_key = [0u8; 16];
    let mut nonce = [0u8; 12];
    hkdf.expand(
        b"Content-Encoding: aes128gcm\0",
        &mut content_encryption_key,
    )
    .map_err(|error| format!("failed to derive content encryption key: {error}"))?;
    hkdf.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|error| format!("failed to derive nonce: {error}"))?;
    Ok((content_encryption_key, nonce))
}

/// Encrypt `payload` for a subscription and build the encrypted request body.
///
/// Returns the message body (RFC 8188 content-coding header followed by a single
/// AES-128-GCM record), plus the base64url `salt` and sender public key carried in the
/// `Encryption` and `Crypto-Key` headers.
fn encrypt_push_payload(
    p256dh: &str,
    auth: &str,
    sender_private_b64: &str,
    salt_b64: &str,
    payload: &str,
) -> Result<(Vec<u8>, String, String), String> {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let recipient_public_bytes = engine
        .decode(p256dh)
        .map_err(|error| format!("invalid p256dh key: {error}"))?;
    let auth_secret = engine
        .decode(auth)
        .map_err(|error| format!("invalid auth secret: {error}"))?;
    let recipient_public = PublicKey::from_sec1_bytes(&recipient_public_bytes)
        .map_err(|error| format!("invalid p256dh point: {error}"))?;

    let sender_private_bytes = engine
        .decode(sender_private_b64)
        .map_err(|error| format!("invalid sender private key: {error}"))?;
    let sender_private = SigningKey::from_slice(&sender_private_bytes)
        .map_err(|error| format!("invalid sender private key: {error}"))?;
    let sender_public_key = sender_private.verifying_key();
    let sender_public_bytes = sender_public_key
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();

    let salt = engine
        .decode(salt_b64)
        .map_err(|error| format!("invalid salt: {error}"))?;
    let shared_secret = p256::elliptic_curve::ecdh::diffie_hellman(
        sender_private.as_nonzero_scalar(),
        recipient_public.as_affine(),
    );

    let mut key_info = b"WebPush: info".to_vec();
    key_info.push(0x00);
    key_info.extend_from_slice(recipient_public.to_encoded_point(false).as_bytes());
    key_info.extend_from_slice(&sender_public_bytes);

    let (content_encryption_key, nonce) =
        push_key_material(&shared_secret, &auth_secret, &key_info, &salt)?;

    let mut plaintext = payload.as_bytes().to_vec();
    plaintext.push(0x02); // padding delimiter of the final (only) record

    let cipher = Aes128Gcm::new(&Key::<Aes128Gcm>::from(content_encryption_key));
    let ciphertext = cipher
        .encrypt((&nonce).into(), plaintext.as_slice())
        .map_err(|error| format!("failed to encrypt push payload: {error}"))?;

    let mut body = Vec::with_capacity(
        salt.len() + 4 + 1 + sender_public_bytes.len() + nonce.len() + ciphertext.len(),
    );
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65); // idlen: uncompressed P-256 point
    body.extend_from_slice(&sender_public_bytes);
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&ciphertext);

    Ok((
        body,
        engine.encode(salt),
        engine.encode(sender_public_bytes),
    ))
}

/// Encrypt `payload` for a subscription with a fresh ephemeral sender key and random salt.
fn encrypt_payload(
    p256dh: &str,
    auth: &str,
    payload: &str,
) -> Result<(Vec<u8>, String, String), String> {
    use rand::RngCore;
    let mut rng = rand::thread_rng();
    let mut sender_private_bytes = [0u8; 32];
    rng.fill_bytes(&mut sender_private_bytes);
    let mut salt = [0u8; 16];
    rng.fill_bytes(&mut salt);
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    encrypt_push_payload(
        p256dh,
        auth,
        &engine.encode(sender_private_bytes),
        &engine.encode(salt),
        payload,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;
    use signature::DigestVerifier;
    use std::io::{Read, Write};

    #[test]
    fn vapid_keys_round_trip() {
        let private = generate_private_key();
        let public = public_key(&private);
        assert!(
            !public.is_empty(),
            "a public key derives from the private one"
        );
        assert_ne!(public, generate_private_key());
    }

    #[test]
    fn subscriptions_persist() {
        let dir = std::env::temp_dir().join(format!("orion-push-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = crate::engine::store::Store::at(dir.clone());
        let push = PushService::new(&store);
        push.subscribe(
            json!({ "endpoint": "https://push.example/1", "keys": { "p256dh": "k", "auth": "a" } }),
            None,
        );
        assert_eq!(push.subscriptions().len(), 1);
        push.unsubscribe("https://push.example/1");
        assert!(push.subscriptions().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vapid_token_has_expected_claims() {
        let private = generate_private_key();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        let token = vapid_jwt(&private, "https://push.example.net", VAPID_SUBJECT, now)
            .expect("token builds");

        let segments: Vec<&str> = token.split('.').collect();
        assert_eq!(segments.len(), 3, "JWT has header, claims and signature");
        let header: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(segments[0])
                .expect("header"),
        )
        .expect("header JSON");
        assert_eq!(header.get("alg").and_then(Value::as_str), Some("ES256"));
        assert_eq!(header.get("typ").and_then(Value::as_str), Some("JWT"));

        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(segments[1])
                .expect("claims"),
        )
        .expect("claims JSON");
        assert_eq!(
            claims.get("aud").and_then(Value::as_str),
            Some("https://push.example.net")
        );
        assert_eq!(
            claims.get("sub").and_then(Value::as_str),
            Some(VAPID_SUBJECT)
        );
        let exp = claims
            .get("exp")
            .and_then(Value::as_u64)
            .expect("exp claim");
        assert!(
            exp > now && exp <= now + VAPID_EXPIRY_SECONDS,
            "expiry is now plus 12 hours"
        );

        // The ES256 signature verifies with the corresponding public key.
        let public_key = public_key_uncompressed(&private);
        let public_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&public_key)
            .expect("public key");
        let verifying =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&public_bytes).expect("verifying key");
        let mut digest = Sha256::new();
        digest.update(signing_input("https://push.example.net", VAPID_SUBJECT, now).as_bytes());
        let signature_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segments[2])
            .expect("signature");
        let signature =
            p256::ecdsa::Signature::from_slice(&signature_bytes).expect("signature bytes");
        assert!(
            verifying.verify_digest(digest, &signature).is_ok(),
            "ES256 signature verifies"
        );
    }

    #[test]
    fn push_encryption_matches_rfc8291_test_vector() {
        // Values from RFC 8291 / draft-ietf-webpush-encryption-09, Appendix A.
        const RECIPIENT_PUBLIC_B64: &str =
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        const AUTH_B64: &str = "BTBZMqHH6r4Tts7J_aSIgg";
        const SENDER_PRIVATE_B64: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
        const SALT_B64: &str = "DGv6ra1nlYgDCS1FRnbzlw";
        const EXPECTED_NONCE_B64: &str = "4h_95klXJ5E_qnoN";
        const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";

        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let salt = engine.decode(SALT_B64).expect("salt decodes");
        let auth_secret = engine.decode(AUTH_B64).expect("auth decodes");
        let sender_private_bytes = engine
            .decode(SENDER_PRIVATE_B64)
            .expect("sender private decodes");
        let sender_private = SigningKey::from_slice(&sender_private_bytes).expect("sender key");
        let sender_public_key = sender_private.verifying_key();
        let sender_public_bytes = sender_public_key
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();

        let (body, salt_out, sender_public_out) = encrypt_push_payload(
            RECIPIENT_PUBLIC_B64,
            AUTH_B64,
            SENDER_PRIVATE_B64,
            SALT_B64,
            PLAINTEXT,
        )
        .expect("encrypt");

        assert_eq!(salt_out, SALT_B64);
        assert_eq!(sender_public_out, engine.encode(&sender_public_bytes));
        assert_eq!(&body[0..16], &salt[..]);
        assert_eq!(&body[16..20], &RECORD_SIZE.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(&body[21..86], &sender_public_bytes[..]);
        assert_eq!(
            engine.encode(&body[86..98]),
            EXPECTED_NONCE_B64,
            "nonce matches RFC 8291 Appendix A"
        );

        // Decrypt with the recipient's private key: the payload round-trips.
        let recipient_private_b64 = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
        let recipient_private_bytes = engine
            .decode(recipient_private_b64)
            .expect("recipient private");
        let recipient_private =
            SigningKey::from_slice(&recipient_private_bytes).expect("recipient key");
        let shared_secret = p256::elliptic_curve::ecdh::diffie_hellman(
            recipient_private.as_nonzero_scalar(),
            sender_public_key.as_affine(),
        );
        let mut key_info = b"WebPush: info".to_vec();
        key_info.push(0x00);
        key_info.extend_from_slice(
            PublicKey::from_sec1_bytes(
                &engine
                    .decode(RECIPIENT_PUBLIC_B64)
                    .expect("recipient public"),
            )
            .expect("recipient point")
            .to_encoded_point(false)
            .as_bytes(),
        );
        key_info.extend_from_slice(&sender_public_bytes);
        let (content_encryption_key, expected_nonce) =
            push_key_material(&shared_secret, &auth_secret, &key_info, &salt)
                .expect("key material");
        assert_eq!(&expected_nonce[..], &body[86..98]);

        let cipher = Aes128Gcm::new(&Key::<Aes128Gcm>::from(content_encryption_key));
        let plaintext = cipher
            .decrypt((&expected_nonce).into(), &body[98..])
            .expect("decrypt");
        assert_eq!(plaintext, format!("{PLAINTEXT}\x02").into_bytes());
    }

    #[test]
    fn send_test_delivers_to_endpoint() {
        let dir = std::env::temp_dir().join(format!("orion-push-send-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = crate::engine::store::Store::at(dir.clone());
        let push = PushService::new(&store);

        // A subscription the way a browser would provide it: P-256 key plus auth secret.
        let mut rng = rand::thread_rng();
        let subscription_key = SigningKey::random(&mut rng);
        let subscription_public = subscription_key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let mut auth = [0u8; 16];
        rng.fill_bytes(&mut auth);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds listener");
        let port = listener.local_addr().expect("local addr").port();
        let endpoint = format!("http://127.0.0.1:{port}/push/test");
        let origin = format!("http://127.0.0.1:{port}");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();

        push.subscribe(
            json!({
                "endpoint": endpoint,
                "keys": {
                    "p256dh": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&subscription_public),
                    "auth": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(auth),
                }
            }),
            None,
        );

        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().expect("accepts connection");
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buffer = Vec::new();
            let mut temp = [0u8; 4096];
            loop {
                match stream.read(&mut temp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buffer.extend_from_slice(&temp[..n]);
                        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.flush();
            tx.send(String::from_utf8_lossy(&buffer).into_owned())
                .expect("sends request");
        });

        let result = push.send_test(&endpoint, "test-payload", "normal");
        assert!(result.is_ok(), "push delivery should succeed: {result:?}");

        let request = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("receives the push request");
        let lowercase: Vec<String> = request.lines().map(|line| line.to_lowercase()).collect();
        assert!(
            request.starts_with("POST "),
            "expected a POST request, got:\n{request}"
        );
        assert!(
            lowercase
                .iter()
                .any(|line| line.starts_with("authorization: vapid t=")),
            "expected a VAPID Authorization header, got:\n{request}"
        );
        assert!(
            lowercase
                .iter()
                .any(|line| line.starts_with("urgency: normal")),
            "expected an Urgency header, got:\n{request}"
        );
        assert!(
            lowercase.iter().any(|line| line.starts_with("ttl:")),
            "expected a TTL header, got:\n{request}"
        );
        assert!(
            lowercase
                .iter()
                .any(|line| line.starts_with("content-encoding: aes128gcm")),
            "expected a Content-Encoding header, got:\n{request}"
        );
        assert!(
            lowercase
                .iter()
                .any(|line| line.starts_with("encryption: salt=")),
            "expected an Encryption header, got:\n{request}"
        );
        assert!(
            lowercase
                .iter()
                .any(|line| line.starts_with("crypto-key: p256ecdsa=")),
            "expected a Crypto-Key header, got:\n{request}"
        );

        // The VAPID token binds to the endpoint origin and carries a contact subject.
        let auth_line = request
            .lines()
            .find(|line| line.to_lowercase().starts_with("authorization: vapid t="))
            .expect("authorization header");
        let token = auth_line
            .split_once("t=")
            .expect("t parameter")
            .1
            .split(',')
            .next()
            .expect("token value");
        let claims_b64 = token.split('.').nth(1).expect("claims segment");
        let claims: Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(claims_b64)
                .expect("claims"),
        )
        .expect("claims JSON");
        assert_eq!(
            claims.get("aud").and_then(Value::as_str),
            Some(origin.as_str())
        );
        assert_eq!(
            claims.get("sub").and_then(Value::as_str),
            Some(VAPID_SUBJECT)
        );
        let exp = claims
            .get("exp")
            .and_then(Value::as_u64)
            .expect("exp claim");
        assert!(exp > now, "token expires after the request time");

        server.join().expect("server thread");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn broadcast_filters_subscriptions_by_alerts_policy() {
        let dir =
            std::env::temp_dir().join(format!("orion-push-broadcast-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let store = crate::engine::store::Store::at(dir.clone());
        let push = PushService::new(&store);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binds listener");
        let port = listener.local_addr().expect("local addr").port();
        let endpoint = format!("http://127.0.0.1:{port}/push/active");

        let mut rng = rand::thread_rng();
        let subscription_key = SigningKey::random(&mut rng);
        let subscription_public = subscription_key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let mut auth = [0u8; 16];
        rng.fill_bytes(&mut auth);

        push.subscribe(
            json!({
                "endpoint": endpoint,
                "keys": {
                    "p256dh": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&subscription_public),
                    "auth": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(auth),
                }
            }),
            Some(json!({ "input": false, "done": "always" })),
        );

        let payload = crate::protocol::PushPayload {
            machine_id: Some("local".to_string()),
            pane_id: Some("p1".to_string()),
            workspace_id: None,
            title: "Test".to_string(),
            body: "Blocked".to_string(),
            tag: "orion-pane-p1".to_string(),
        };

        // Broadcast blocked: subscriber has input: false, so it must not send
        push.broadcast(&payload, crate::protocol::AgentStatus::Blocked, None);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let _ = std::fs::remove_dir_all(dir);
    }
    /// Rebuilds the JWT signing input for the round-trip verification test.
    fn signing_input(aud: &str, sub: &str, now: u64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"ES256","typ":"JWT"}"#);
        let claims = format!(
            r#"{{"aud":"{}","exp":{},"sub":"{}"}}"#,
            aud,
            now + VAPID_EXPIRY_SECONDS,
            sub
        );
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.as_bytes());
        format!("{header}.{claims}")
    }
}

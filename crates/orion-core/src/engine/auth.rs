//! The optional shared-token gate for remote access.
//!
//! No user database: the token is the whole authorization decision. Compares are constant-time
//! (`subtle::ConstantTimeEq`), and the cookie is HttpOnly + SameSite=Strict. An empty token
//! disables the gate; loopback requests are then allowed (see `access`).

use subtle::ConstantTimeEq;

pub const TOKEN_COOKIE: &str = "orion_web_token";
pub const DEVICE_COOKIE: &str = "orion_web_device";
pub const COOKIE_MAX_AGE_SECONDS: i64 = 31_536_000;

/// Constant-time token match (length first, then bytes).
pub fn token_matches(candidate: &str, token: &str) -> bool {
    if candidate.len() != token.len() {
        return false;
    }
    candidate.as_bytes().ct_eq(token.as_bytes()).into()
}

pub fn parse_cookies(header: &str) -> std::collections::HashMap<String, String> {
    let mut cookies = std::collections::HashMap::new();
    for pair in header.split(';') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let value = value.trim();
        let decoded = percent_decode(value);
        cookies.insert(name.to_string(), decoded);
    }
    cookies
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = |byte: u8| -> Option<u8> {
                    match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        b'A'..=b'F' => Some(byte - b'A' + 10),
                        _ => None,
                    }
                };
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push(high * 16 + low);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Whether a request is allowed without credentials (loopback on desktop is always local).
pub fn requires_auth(path: &str) -> bool {
    if path == "/ws" {
        return true;
    }
    if !path.starts_with("/api/") {
        return false;
    }
    path != "/api/health" && path != "/api/auth" && path != "/api/devices/pair"
}

/// The `Authorization: Bearer` or token-cookie credential, if any.
pub fn presented_token(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    if let Some(authorization) = headers.get("authorization") {
        if let Some(token) = authorization
            .strip_prefix("Bearer ")
            .or_else(|| authorization.strip_prefix("bearer "))
        {
            return Some(token.to_string());
        }
    }
    headers
        .get("cookie")
        .and_then(|header| parse_cookies(header).remove(TOKEN_COOKIE))
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{TOKEN_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE_SECONDS}",
        percent_encode(token)
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_compare_is_constant_time_and_exact() {
        assert!(token_matches("s3cret-token", "s3cret-token"));
        assert!(!token_matches("s3cret-tokem", "s3cret-token"));
        assert!(!token_matches("short", "a-much-longer-token"));
    }

    #[test]
    fn cookies_parse_and_reject_junk() {
        let cookies = parse_cookies("orion_web_token=abc; other=1; broken");
        assert_eq!(
            cookies.get("orion_web_token").map(String::as_str),
            Some("abc")
        );
        assert_eq!(cookies.get("other").map(String::as_str), Some("1"));
    }

    #[test]
    fn auth_requires_everything_except_health_auth_and_pairing() {
        assert!(requires_auth("/api/pane/read"));
        assert!(requires_auth("/ws"));
        assert!(!requires_auth("/api/health"));
        assert!(!requires_auth("/api/auth"));
        assert!(!requires_auth("/api/devices/pair"));
        assert!(!requires_auth("/"));
    }
}

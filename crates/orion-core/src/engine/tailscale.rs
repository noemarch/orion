//! What Tailscale on this PC can already do for a phone, read-only.
//!
//! `tailscale status --json` and `tailscale serve status --json` only — never `serve`, `up`, or
//! anything else that changes the tailnet (and never `funnel`, which would expose the all).
//! The Phone panel turns the answer into the address that already works, or the one command
//! the user still has to run on this PC.

use std::process::{Command, Stdio};

use serde_json::Value;

const LOOPBACK: [&str; 4] = ["127.0.0.1", "localhost", "[::1]", "::1"];
const HTTPS_PORTS: [u16; 4] = [443, 8443, 7317, 17317];

/// The two commands' output; `None` where the command failed or is not installed.
#[derive(Debug, Clone)]
pub struct TailscaleOutput {
    pub status: Option<String>,
    pub serve: Option<String>,
}

fn none_access(state: &str) -> Value {
    serde_json::json!({
        "state": state,
        "dns_name": null,
        "serving_url": null,
        "serve_command": null,
        "serve_url": null,
    })
}

pub fn parse_node_name(status: &str) -> Option<String> {
    let value: Value = serde_json::from_str(status).ok()?;
    value
        .get("Self")?
        .get("DNSName")?
        .as_str()
        .map(|name| name.trim_end_matches('.').to_string())
        .filter(|name| !name.is_empty())
}

pub fn parse_tailscale_ip(status: &str) -> Option<String> {
    let value: Value = serde_json::from_str(status).ok()?;
    value
        .get("Self")?
        .get("TailscaleIPs")?
        .as_array()?
        .iter()
        .filter_map(|ip| ip.as_str())
        .find(|ip| {
            let parts: Vec<&str> = ip.split('.').collect();
            parts.len() == 4
                && parts
                    .iter()
                    .all(|part| part.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
}

fn proxies_to(target: Option<&str>, port: u16) -> bool {
    let Some(target) = target else {
        return false;
    };
    let Some(after_scheme) = target.split("://").nth(1) else {
        return false;
    };
    let (host, path) = after_scheme.split_once('/').unwrap_or((after_scheme, "/"));
    let (hostname, target_port) = match host.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().unwrap_or(0)),
        None => (
            host,
            if target.starts_with("https:") {
                443
            } else {
                80
            },
        ),
    };
    LOOPBACK.contains(&hostname) && target_port == port && (path.is_empty() || path == "/")
}

fn https_url(host: &str, port: u16) -> String {
    if port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{port}")
    }
}

/// Pure: the two commands' output (or their absence) to what the phone panel needs.
pub fn parse_tailscale(output: &TailscaleOutput, port: u16) -> Value {
    let Some(status_text) = &output.status else {
        return none_access("missing");
    };
    let Ok(status) = serde_json::from_str::<Value>(status_text) else {
        return none_access("stopped");
    };
    if status.get("BackendState").and_then(Value::as_str) != Some("Running") {
        return none_access("stopped");
    }
    let dns = parse_node_name(status_text);
    let serve: Value = output
        .serve
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null);
    let taken: Vec<u16> = serve
        .get("TCP")
        .and_then(Value::as_object)
        .map(|tcp| tcp.keys().filter_map(|key| key.parse().ok()).collect())
        .unwrap_or_default();
    let mut serving_url = None;
    if let Some(web) = serve.get("Web").and_then(Value::as_object) {
        for (host_port, site) in web {
            let Some(separator) = host_port.rfind(':') else {
                continue;
            };
            let (host, web_port) = (&host_port[..separator], &host_port[separator + 1..]);
            let Ok(web_port) = web_port.parse::<u16>() else {
                continue;
            };
            // An HTTP listener is no use to a phone.
            if serve
                .get("TCP")
                .and_then(|tcp| tcp.get(web_port.to_string()))
                .and_then(|entry| entry.get("HTTPS"))
                .and_then(Value::as_bool)
                != Some(true)
            {
                continue;
            }
            let proxy = site
                .get("Handlers")
                .and_then(|handlers| handlers.get("/"))
                .and_then(|handler| handler.get("Proxy"))
                .and_then(Value::as_str);
            if proxies_to(proxy, port) {
                serving_url = Some(https_url(host, web_port));
                break;
            }
        }
    }
    let free = if serving_url.is_none() {
        HTTPS_PORTS
            .iter()
            .find(|candidate| !taken.contains(candidate))
            .copied()
    } else {
        None
    };
    serde_json::json!({
        "state": "running",
        "dns_name": dns,
        "serving_url": serving_url,
        "serve_command": free.map(|free| format!("tailscale serve --bg --https={free} http://127.0.0.1:{port}")),
        "serve_url": free
            .and_then(|free| dns.clone().map(|dns| https_url(&dns, free))),
    })
}

/// Finds `tailscale` on PATH (plus the macOS App Store location).
pub fn tailscale_binary() -> Option<std::path::PathBuf> {
    crate::agents::find_in_path("tailscale")
}

fn run(binary: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

/// Reads both commands. Returns `None` when tailscale is not installed.
pub fn read_tailscale() -> Option<TailscaleOutput> {
    let binary = tailscale_binary()?;
    Some(TailscaleOutput {
        status: run(&binary, &["status", "--json"]),
        serve: run(&binary, &["serve", "status", "--json"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = r#"{"BackendState":"Running","Self":{"DNSName":"orion.example.ts.net.","TailscaleIPs":["100.101.102.103","fd7a:115c:a1e0::1"]},"User":{}}"#;

    #[test]
    fn running_status_names_the_node_and_picks_a_free_https_port() {
        let access = parse_tailscale(
            &TailscaleOutput {
                status: Some(RUNNING.to_string()),
                serve: Some(r#"{"TCP":{"443":{"HTTPS":true}},"Web":{}}"#.to_string()),
            },
            7317,
        );
        assert_eq!(access["state"], "running");
        assert_eq!(access["dns_name"], "orion.example.ts.net");
        assert!(access["serving_url"].is_null());
        assert!(access["serve_command"]
            .as_str()
            .unwrap()
            .contains("--https=8443"));
        assert_eq!(
            parse_tailscale_ip(RUNNING).as_deref(),
            Some("100.101.102.103")
        );
    }

    #[test]
    fn stopped_and_missing_report_their_state() {
        let stopped = parse_tailscale(
            &TailscaleOutput {
                status: Some(r#"{"BackendState":"Stopped"}"#.to_string()),
                serve: None,
            },
            7317,
        );
        assert_eq!(stopped["state"], "stopped");
        let missing = parse_tailscale(
            &TailscaleOutput {
                status: None,
                serve: None,
            },
            7317,
        );
        assert_eq!(missing["state"], "missing");
    }
}

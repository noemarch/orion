//! `orion serve` must start an Engine-backed server on an ephemeral port without seeding a
//! workspace. This is the hook the conformance harness spawns.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_server() -> Server {
    let dir = std::env::temp_dir().join(format!("orion-serve-{}", uuid::Uuid::new_v4()));
    let state = dir.join("state");
    std::fs::create_dir_all(&state).expect("state dir");

    let mut child = Command::new(env!("CARGO_BIN_EXE_orion"))
        .args(["serve", "--host", "127.0.0.1", "--port", "0", "--state-dir"])
        .arg(&state)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("orion starts");

    let stdout = child.stdout.take().expect("stdout is piped");
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let ready: Value = loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(Ok(line)) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value.get("event").and_then(Value::as_str) == Some("ready") {
                        break value;
                    }
                }
            }
            Ok(Err(error)) => panic!("server stdout: {error}"),
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("orion serve did not print a ready line within 30s");
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                panic!("orion serve exited before becoming ready");
            }
        }
    };

    Server {
        child,
        port: ready["port"].as_u64().expect("port") as u16,
    }
}

fn get(path: &str, port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects to the server");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .expect("writes the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("reads the response");
    response
}

#[test]
fn serve_answers_bridge_health_on_port_zero() {
    let server = start_server();
    let response = get("/api/health?scope=bridge", server.port);
    assert!(
        response.contains("\"authenticated\":true") && response.contains("bridge_protocol"),
        "unexpected health response: {response}"
    );

    let response = get("/api/machines", server.port);
    assert!(
        response.contains("\"id\":\"local\""),
        "unexpected machines response: {response}"
    );
}

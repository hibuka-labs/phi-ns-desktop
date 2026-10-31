//! End-to-end smoke for the sidecar process: mu JSON-lines in, a fake
//! `cua-driver mcp` child out. The real device does the rest (D9); this pins
//! the framing, the mapping hand-off, the deadline kill, and the respawn.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A `cua-driver mcp` stand-in: JSON-RPC over stdio, canned tool answers,
/// one tool (`press_key`) that hangs so the watchdog can prove itself.
const FAKE_DRIVER: &str = r#"#!/usr/bin/env python3
import json, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    mid = msg["id"]
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {},
                  "serverInfo": {"name": "fake-driver", "version": "0"}}
    elif method == "tools/call":
        tool = msg["params"]["name"]
        args = msg["params"].get("arguments", {})
        if tool == "get_desktop_state":
            payload = {"capture_id": "cap_fake",
                       "screenshot_file_path": args.get("screenshot_out_file", "/tmp/x.png"),
                       "screenshot_mime_type": "image/png",
                       "screenshot_width": 100, "screenshot_height": 50}
        elif tool == "click":
            payload = {"effect": "confirmed",
                       "capture_id_echo": args.get("capture_id", "")}
        elif tool == "press_key":
            time.sleep(30)
            payload = {"effect": "confirmed"}
        else:
            payload = {"effect": "confirmed"}
        result = {"content": [{"type": "text", "text": json.dumps(payload)}],
                  "structuredContent": payload}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#;

struct Sidecar {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Sidecar {
    fn start(driver: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_phi-ns-cua-stub"))
            .arg("--driver")
            .arg(driver)
            .arg("--shots")
            .arg(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn the stub");
        let stdin = child.stdin.take().expect("stub stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stub stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Send one mu request and read one mu reply.
    fn roundtrip(&mut self, frame: &str) -> serde_json::Value {
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .and_then(|_| self.stdin.flush())
            .expect("write request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read reply");
        serde_json::from_str(&line).expect("reply is JSON")
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fake_driver() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("phi-ns-cua-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake-cua-driver");
    std::fs::write(&path, FAKE_DRIVER).unwrap();
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&path, perm).unwrap();
    path
}

#[test]
fn sidecar_serves_mu_framing_and_survives_a_deadline() {
    let driver = fake_driver();
    let mut stub = Sidecar::start(&driver);

    // A look: shaped handle for the pack's ImageHandle, id echoed for audit.
    let reply = stub.roundtrip(
        r#"{"op":"desk.shot","_request_id":1,"timeout_ms":2000,"mode":"desktop","window":null,"region":null}"#,
    );
    assert_eq!(reply["_request_id"], 1, "{reply}");
    let result = &reply["result"];
    assert!(result["path"].as_str().unwrap().contains("png"));
    assert_eq!(result["revision"], "cap_fake");
    assert_eq!(result["width"], 100);

    // An act rides the look's native capture; the reply echoes the request id.
    let reply = stub.roundtrip(
        r#"{"op":"desk.click","_request_id":2,"timeout_ms":2000,"target":[10,20],"button":"left"}"#,
    );
    assert_eq!(reply["_request_id"], 2, "{reply}");
    assert_eq!(reply["result"]["effect"], "confirmed");
    assert_eq!(reply["result"]["capture_id_echo"], "cap_fake");

    // A tool that never answers becomes a classified deadline — an act that
    // may have landed is never a refusal.
    let start = Instant::now();
    let reply =
        stub.roundtrip(r#"{"op":"desk.key","_request_id":3,"timeout_ms":300,"key":"enter"}"#);
    assert_eq!(reply["_request_id"], 3, "{reply}");
    assert_eq!(reply["error"]["code"], "ACTION_OUTCOME_UNKNOWN", "{reply}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the stub must not hang on a wedged driver"
    );

    // The child died with the deadline; the next look reconnects and works.
    let reply = stub.roundtrip(
        r#"{"op":"desk.shot","_request_id":4,"timeout_ms":2000,"mode":"desktop","window":null,"region":null}"#,
    );
    assert_eq!(reply["_request_id"], 4, "{reply}");
    assert_eq!(reply["result"]["revision"], "cap_fake");
}

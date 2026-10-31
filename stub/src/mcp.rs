//! JSON-RPC 2.0 client over a `cua-driver mcp` child's stdio.
//!
//! Newline-delimited frames, no metadata re-sent after the handshake. The
//! child runs unrestricted (env contract below): cua's own consent windows
//! never open — approval lives at the host Gate (sdk-assembly §3.5/§4.3).

use phi_wire_protocol::Error;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Protocol version we hand the driver; it may answer with its own.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The driver child, killable from a watchdog while a call is blocked on it.
pub type ChildSlot = Arc<Mutex<Option<Child>>>;

/// A live driver child plus its in-flight JSON-RPC counter. The child itself
/// lives in the caller's [`ChildSlot`] so a watchdog can kill it mid-call.
pub struct Mcp {
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    /// Spawn `cua-driver mcp` into `child` and finish the initialize handshake.
    pub fn spawn(driver: &str, extra: &[String], child: ChildSlot) -> Result<Mcp, Error> {
        let mut cmd = Command::new(driver);
        cmd.arg("mcp")
            .args(extra)
            // Programmatic contract: no modal, no banner — the host Gate is
            // the only approval surface (docs/driver-permission-modes-and-consent-plan).
            .env("CUA_DRIVER_PERMISSION_MODE", "unrestricted")
            .env("CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut spawned = cmd
            .spawn()
            .map_err(|e| Error::new("SETUP_REQUIRED", format!("cannot spawn {driver} mcp: {e}")))?;
        let stdin = spawned
            .stdin
            .take()
            .ok_or_else(|| Error::new("SETUP_REQUIRED", "driver stdin unavailable"))?;
        let stdout = spawned
            .stdout
            .take()
            .ok_or_else(|| Error::new("SETUP_REQUIRED", "driver stdout unavailable"))?;
        *child.lock().unwrap_or_else(|p| p.into_inner()) = Some(spawned);
        let mut mcp = Mcp {
            stdin,
            reader: BufReader::new(stdout),
            next_id: 0,
        };
        mcp.handshake()?;
        Ok(mcp)
    }

    /// Kill whatever child is armed — the watchdog's only move.
    pub fn kill(child: &ChildSlot) {
        if let Ok(mut guard) = child.lock()
            && let Some(c) = guard.as_mut()
        {
            let _ = c.kill();
        }
    }

    fn handshake(&mut self) -> Result<(), Error> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "phi-ns-cua-stub", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        // The spec's ready signal; the driver may answer nothing, which the
        // request loop below treats as "no frame for this id" and skips.
        self.notify("notifications/initialized", json!({}))
    }

    /// One `tools/call`, unpacked to the tool's payload.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, Error> {
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(classify_error(&tool_text(&result)));
        }
        if let Some(structured) = result.get("structuredContent") {
            return Ok(structured.clone());
        }
        let text = tool_text(&result);
        // cua texts its payloads as JSON; anything else is surfaced as text.
        Ok(serde_json::from_str(&text).unwrap_or(json!({ "text": text })))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        self.send_line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = self.read_line()?;
            let msg: Value = serde_json::from_str(&line).map_err(|e| {
                Error::new(
                    "PROTOCOL_MISMATCH",
                    format!("driver wrote non-JSON frame: {e}"),
                )
            })?;
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                // Notification or a reply to nothing we still hold — skip.
                continue;
            }
            if let Some(err) = msg.get("error") {
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("driver tool error");
                return Err(classify_error(message));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), Error> {
        self.send_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn send_line(&mut self, frame: &Value) -> Result<(), Error> {
        let wire = serde_json::to_vec(frame)
            .map_err(|e| Error::new("PROTOCOL_MISMATCH", format!("cannot encode frame: {e}")))?;
        self.stdin
            .write_all(&wire)
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| dead_driver(&e))
    }

    fn read_line(&mut self) -> Result<String, Error> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|e| dead_driver(&e))?;
        if n == 0 {
            return Err(Error::new("DEVICE_OFFLINE", "driver closed its stdout"));
        }
        Ok(line)
    }
}

/// The driver child died or was killed under a deadline.
fn dead_driver(e: &std::io::Error) -> Error {
    Error::new("DEVICE_OFFLINE", format!("driver channel failed: {e}"))
}

fn tool_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Map a driver-side failure onto the wire's own vocabulary (inventory §3.3):
/// every bucket says either "nothing was applied, retry" or "outcome unknown".
pub fn classify_error(msg: &str) -> Error {
    let lower = msg.to_lowercase();
    let code = if lower.contains("stale") {
        "STALE_TARGET"
    } else if lower.contains("invalid") || lower.contains("argument") || lower.contains("schema") {
        "INVALID_ARGUMENT"
    } else if lower.contains("unsupported") {
        "UNSUPPORTED_CAPABILITY"
    } else if lower.contains("not_found")
        || lower.contains("not found")
        || lower.contains("target_missing")
        || lower.contains("no_window")
        || lower.contains("no window")
    {
        "TARGET_NOT_FOUND"
    } else if lower.contains("foreground") {
        "TARGET_NOT_FOREGROUND"
    } else if lower.contains("occluded") {
        "TARGET_OCCLUDED"
    } else if lower.contains("permission") || lower.contains("consent") {
        "ACTION_REJECTED"
    } else if lower.contains("unknown") || lower.contains("unverifiable") {
        "ACTION_OUTCOME_UNKNOWN"
    } else {
        "ACTION_REJECTED"
    };
    Error::new(code, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every driver-side failure lands in a wire bucket that says whether
    /// anything was applied (inventory §3.3).
    #[test]
    fn driver_failures_bucket_into_wire_codes() {
        for (msg, code) in [
            ("element is stale", "STALE_TARGET"),
            ("invalid arguments for click", "INVALID_ARGUMENT"),
            ("schema mismatch in request", "INVALID_ARGUMENT"),
            ("unsupported operation", "UNSUPPORTED_CAPABILITY"),
            ("window not found", "TARGET_NOT_FOUND"),
            ("target_missing on screen", "TARGET_NOT_FOUND"),
            ("window is not foreground", "TARGET_NOT_FOREGROUND"),
            ("window is occluded", "TARGET_OCCLUDED"),
            ("requires user permission", "ACTION_REJECTED"),
            ("consent was withdrawn", "ACTION_REJECTED"),
            ("outcome unknown after crash", "ACTION_OUTCOME_UNKNOWN"),
            ("something else entirely", "ACTION_REJECTED"),
        ] {
            assert_eq!(classify_error(msg).code, code, "msg={msg}");
        }
    }
}

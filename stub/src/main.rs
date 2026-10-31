//! cua wire stub sidecar (sdk-assembly §9.7): phi-wire-protocol JSON-lines in,
//! `cua-driver mcp` out. One process, one driver child, one request at a time.
//!
//! The stub is a translator and nothing else: consent lives at the host Gate,
//! capture bookkeeping lives at the host ledger, and a hung driver is a
//! classified deadline — never a hang.

mod map;
mod mcp;

use map::State;
use mcp::{ChildSlot, Mcp};
use phi_wire_protocol::{Reply, Request};
use serde_json::{Map, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default per-call deadline when the wire asks for none.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Watchdog tick: fine enough to notice a deadline, cheap enough to ignore.
const TICK: Duration = Duration::from_millis(50);

fn main() {
    let cfg = Config::from_env();
    if let Err(e) = serve(&cfg) {
        eprintln!("phi-ns-cua-stub: {}", e);
        std::process::exit(1);
    }
}

/// Where the stub runs and what it wraps.
struct Config {
    /// `cua-driver` binary (or a wrapper path).
    driver: String,
    /// Extra arguments passed to the driver before `mcp`.
    driver_args: Vec<String>,
    /// Directory captures are written to.
    shots: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        let mut driver_args = Vec::new();
        let mut driver = std::env::var("CUA_DRIVER_BIN").unwrap_or_else(|_| "cua-driver".into());
        let mut shots = std::env::var("CUA_STUB_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("phi-ns-cua-shots"));
        let mut rest = std::env::args().skip(1);
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--driver" => {
                    if let Some(v) = rest.next() {
                        driver = v;
                    }
                }
                "--shots" => {
                    if let Some(v) = rest.next() {
                        shots = PathBuf::from(v);
                    }
                }
                "--" => {
                    driver_args.extend(rest.by_ref());
                    break;
                }
                other => driver_args.push(other.to_string()),
            }
        }
        let _ = std::fs::create_dir_all(&shots);
        Self {
            driver,
            driver_args,
            shots,
        }
    }
}

/// One in-flight call's deadline; the watchdog kills the child when it passes.
struct Watchdog {
    job: Arc<Mutex<Option<(u64, Instant)>>>,
    done: Arc<AtomicU64>,
    next: AtomicU64,
}

impl Watchdog {
    /// Start a watchdog over whatever child the slot holds.
    fn start(slot: ChildSlot) -> Self {
        let job: Arc<Mutex<Option<(u64, Instant)>>> = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicU64::new(0));
        let (watch_job, watch_done) = (Arc::clone(&job), Arc::clone(&done));
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(TICK);
                let expired = {
                    let guard = watch_job.lock().unwrap_or_else(|p| p.into_inner());
                    match *guard {
                        Some((id, deadline)) => {
                            Instant::now() >= deadline && watch_done.load(Ordering::SeqCst) != id
                        }
                        None => false,
                    }
                };
                if expired {
                    Mcp::kill(&slot);
                    *watch_job.lock().unwrap_or_else(|p| p.into_inner()) = None;
                }
            }
        });
        Self {
            job,
            done,
            next: AtomicU64::new(0),
        }
    }

    /// Register a call; returns its id for [`Watchdog::finish`].
    fn begin(&self, timeout: Duration) -> u64 {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        *self.job.lock().unwrap_or_else(|p| p.into_inner()) = Some((id, Instant::now() + timeout));
        id
    }

    /// The call is over — success, error, or the kill we just survived.
    fn finish(&self, id: u64) {
        self.done.store(id, Ordering::SeqCst);
        *self.job.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Serve mu requests on stdin until the channel closes.
fn serve(cfg: &Config) -> Result<(), phi_wire_protocol::Error> {
    let mut state = State::new(cfg.shots.clone());
    let child: ChildSlot = Arc::new(Mutex::new(None::<Child>));
    let watchdog = Watchdog::start(Arc::clone(&child));
    let mut driver = arm(cfg, &child);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line =
            line.map_err(|e| phi_wire_protocol::Error::new("DEVICE_OFFLINE", format!("stdin: {e}")))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Request>(&line) else {
            // Not our vocabulary: say so on stderr and keep the channel alive
            // (the client's own deadline is the backstop for bad senders).
            eprintln!("phi-ns-cua-stub: undecodable frame: {}", line.trim());
            continue;
        };
        if driver.is_none() {
            driver = arm(cfg, &child);
        }
        let (reply, channel_alive) = handle(&mut state, driver.as_mut(), &watchdog, request);
        if !channel_alive {
            // The child died under this call (deadline kill or crash): the
            // next request must re-arm instead of writing to dead stdio.
            driver = None;
        }
        let wire = serde_json::to_vec(&reply).map_err(|e| {
            phi_wire_protocol::Error::new("PROTOCOL_MISMATCH", format!("encode reply: {e}"))
        })?;
        out.write_all(&wire)
            .and_then(|_| out.write_all(b"\n"))
            .and_then(|_| out.flush())
            .map_err(|e| phi_wire_protocol::Error::new("DEVICE_OFFLINE", format!("stdout: {e}")))?;
    }
    Mcp::kill(&child);
    Ok(())
}

/// Answer one request: map to cua calls, shape the reply, echo the id.
/// The bool answers "is the driver channel still usable?".
fn handle(
    state: &mut State,
    mut driver: Option<&mut Mcp>,
    watchdog: &Watchdog,
    request: Request,
) -> (Reply, bool) {
    let mut reply = Reply {
        request_id: request.request_id,
        result: None,
        error: None,
    };
    let op = request.op.clone();
    let params: Map<String, Value> = request.parameters.into_iter().collect();
    let timeout = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    let call_id = watchdog.begin(Duration::from_millis(timeout));
    let mut channel_alive = true;
    let mut call = |tool: &str, args: Value| -> Result<Value, phi_wire_protocol::Error> {
        let Some(driver) = driver.as_deref_mut() else {
            return Err(phi_wire_protocol::Error::new(
                "SETUP_REQUIRED",
                "the cua driver is not running",
            ));
        };
        let result = driver.call(tool, args);
        if matches!(&result, Err(e) if e.code == "DEVICE_OFFLINE") {
            channel_alive = false;
        }
        result
    };
    let outcome = map::dispatch(state, &op, &params, &mut call);
    watchdog.finish(call_id);
    match outcome {
        Ok(result) => reply.result = Some(result),
        Err(error) => reply.error = Some(recast(&op, &params, error)),
    }
    (reply, channel_alive)
}

/// Driver-channel deaths say what the wire needs to know: an act that may
/// have landed is never a refusal, a read never pretends to have landed.
fn recast(op: &str, params: &Map<String, Value>, error: phi_wire_protocol::Error) -> phi_wire_protocol::Error {
    if error.code == "DEVICE_OFFLINE" {
        return map::deadline_error(op, params);
    }
    error
}

/// (Re)connect the driver; kill whatever half-dead child is left first.
/// No session lifecycle here on purpose: cua keys sessions to the transport
/// lease and each connection gets its own implicit session, so the stub sends
/// no public label and never calls start/end — a fixed label would collide
/// across reconnects (only the owning transport may revive an ended name).
fn arm(cfg: &Config, child: &ChildSlot) -> Option<Mcp> {
    Mcp::kill(child);
    match Mcp::spawn(&cfg.driver, &cfg.driver_args, Arc::clone(child)) {
        Ok(mcp) => Some(mcp),
        Err(e) => {
            eprintln!("phi-ns-cua-stub: driver unavailable: {e}");
            None
        }
    }
}

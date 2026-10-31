//! # phi-ns-desktop — the `desk.*` capability pack
//!
//! The desktop face over the cua wire stub (O10 inventory): capture the
//! screen or a window's accessibility tree, act on element tokens and
//! points, read/write the clipboard, steer windows. What the model writes
//! is code against `desk.*`:
//!
//! ```python
//! state = desk.win_state()
//! for el in state.els:
//!     if el.text == "Save":
//!         el.click()
//!         break
//! shot = desk.shot()                   # -> ImageHandle
//! ```
//!
//! The pack speaks only the `phi-ns` contract over [`Transport`]; assembly
//! (`.with_ns`) and policy live above it (§9.3: `phi-ns-* ↛ codexec`). The
//! wire is `phi-wire-protocol` with **the declared method name as `op`** — the
//! cua stub is ours, so there is no second vocabulary to translate through
//! (one vocabulary, iron rule 2). The stub itself wraps `cua-driver` and
//! never shows cua's own consent windows: approval lives at the host Gate
//! (sdk-assembly §3.5, §4.3).
//!
//! This layer is deliberately thin: target normalization for the worker,
//! the method → wire mapping, and nothing else. Anything the stub can do
//! faithfully, the stub does.

pub mod methods;

use async_trait::async_trait;
use phi_ns::{
    CAPTURE_KEY, HostCall, MethodSpec, NamespaceProvider, NsError, NsObject, WorkerSink,
    remote::Transport,
};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const PY_SHIM: &str = r#"
class ImageHandle:
    """A picture the code can pass around and the model can look at."""
    def __init__(self, d):
        self.path = d.get("path")
        self.mime = d.get("mime", "image/png")
        self.alt = d.get("alt", "")
        # The one-use capture id the host minted for this look — every act on
        # it spends one, and a second act off this look is refused.
        self.capture_id = d.get("capture_id", "")

    def save(self, dst):
        import shutil
        shutil.copyfile(self.path, dst)
        return dst

    def __repr__(self):
        return "ImageHandle(%r)" % self.alt

    def _phi_summary_(self):
        return self.alt


class El:
    """One element handle from desk.win_state: an opaque token plus actions."""
    def __init__(self, d):
        self.token = d.get("token", "")
        self.role = d.get("role", "")
        self.text = d.get("text", "")

    def click(self, button="left"):
        return _phi_call("desk.click", [self._wire(), button])

    def type(self, text):
        return _phi_call("desk.type", [text])

    def __repr__(self):
        return "El(%s, %r)" % (self.role, self.text)

    def _phi_summary_(self):
        return "%s %r" % (self.role, self.text)

    def _wire(self):
        return {"token": self.token}


class WinState:
    """What desk.win_state saw: element handles for one window."""
    def __init__(self, d):
        self.window = d.get("window")
        self.els = [El(e) for e in d.get("els", [])]
        self.raw = d.get("tree")
        # Same one-use capture id as ImageHandle: one look, one act.
        self.capture_id = d.get("capture_id", "")

    def _phi_summary_(self):
        return "%d els" % len(self.els)


def _wire_of(target):
    return target._wire() if isinstance(target, El) else target


class DeskNs:
    def shot(self, mode="desktop", window=None, region=None):
        return ImageHandle(_phi_call("desk.shot", [mode, window, region]))

    def win_state(self, window=None):
        return WinState(_phi_call("desk.win_state", [window]))

    def windows(self):
        return _phi_call("desk.windows", [])

    def apps(self):
        return _phi_call("desk.apps", [])

    def screen(self):
        return _phi_call("desk.screen", [])

    def regions(self, shot=None):
        return _phi_call("desk.regions", [shot])

    def verify(self, claim):
        return _phi_call("desk.verify", [claim])

    def click(self, target, button="left"):
        return _phi_call("desk.click", [_wire_of(target), button])

    def drag(self, start, end):
        return _phi_call("desk.drag", [_wire_of(start), _wire_of(end)])

    def move(self, target):
        return _phi_call("desk.move", [_wire_of(target)])

    def scroll(self, direction, amount=400, target=None):
        return _phi_call("desk.scroll", [direction, amount, _wire_of(target) if target is not None else None])

    def key(self, key):
        return _phi_call("desk.key", [key])

    def hotkey(self, keys):
        return _phi_call("desk.hotkey", [keys])

    def type(self, text):
        return _phi_call("desk.type", [text])

    def menu(self, path):
        return _phi_call("desk.menu", [path])

    def clip_read(self):
        return _phi_call("desk.clip_read", [])

    def clip_write(self, value):
        return _phi_call("desk.clip_write", [value])

    def win_frame(self, window, x, y, w, h):
        return _phi_call("desk.win_frame", [window, x, y, w, h])

    def session(self, op="get"):
        return _phi_call("desk.session", [op])

    def escalate(self):
        return _phi_call("desk.escalate", [])

    def agent_cursor(self, op, **fields):
        return _phi_call("desk.agent_cursor", [op, fields])


desk = DeskNs()
"#;

/// Positional-arg names per declared method — the wire carries named params.
fn param_names(method: &str) -> Result<&'static [&'static str], NsError> {
    Ok(match method {
        "desk.shot" => &["mode", "window", "region"],
        "desk.win_state" => &["window"],
        "desk.windows" => &[],
        "desk.apps" => &[],
        "desk.screen" => &[],
        "desk.regions" => &["shot"],
        "desk.verify" => &["claim"],
        "desk.click" => &["target", "button"],
        "desk.drag" => &["start", "end"],
        "desk.move" => &["target"],
        "desk.scroll" => &["direction", "amount", "target"],
        "desk.key" => &["key"],
        "desk.hotkey" => &["keys"],
        "desk.type" => &["text"],
        "desk.menu" => &["path"],
        "desk.clip_read" => &[],
        "desk.clip_write" => &["value"],
        "desk.win_frame" => &["window", "x", "y", "w", "h"],
        "desk.session" => &["op"],
        "desk.escalate" => &[],
        "desk.agent_cursor" => &["op", "fields"],
        other => return Err(NsError::UndeclaredMethod(other.to_string())),
    })
}

/// Zip positional args onto their names; missing tail args are null.
fn assemble(method: &str, args: &[Value]) -> Result<Map<String, Value>, NsError> {
    let names = param_names(method)?;
    let mut params = Map::new();
    for (i, name) in names.iter().enumerate() {
        params.insert(
            (*name).to_string(),
            args.get(i).cloned().unwrap_or(Value::Null),
        );
    }
    Ok(params)
}

/// After the wire answers: bind what a look saw so the chokepoint can mint
/// the one-use capture id (inventory §3.2). `desk.shot`/`desk.win_state` are
/// the two lookers whose answers yield capture-derived targets; the stub
/// reports what the look bound (geometry, window, tree revision) and unknown
/// fields stay null — the binding is pack-opaque, the ledger only names it.
fn finish(method: &str, mut result: Value) -> Result<Value, NsError> {
    match method {
        "desk.shot" | "desk.win_state" => {
            let obj = result
                .as_object_mut()
                .ok_or_else(|| NsError::Contract(format!("{method} must answer with an object")))?;
            let revision = obj.get("revision").cloned().unwrap_or(Value::Null);
            let width = obj.get("width").cloned().unwrap_or(Value::Null);
            let height = obj.get("height").cloned().unwrap_or(Value::Null);
            let rotation = obj.get("rotation").cloned().unwrap_or(Value::Null);
            let window = obj.get("window").cloned().unwrap_or(Value::Null);
            obj.insert(
                CAPTURE_KEY.into(),
                json!({
                    "revision": revision,
                    "width": width,
                    "height": height,
                    "rotation": rotation,
                    "window": window,
                }),
            );
            Ok(result)
        }
        _ => Ok(result),
    }
}

/// One host-side `desk.*` call: named params out over the wire, answer back.
struct DCall {
    wire: Option<Arc<dyn Transport>>,
    method: &'static str,
}

#[async_trait]
impl HostCall for DCall {
    async fn call(&self, args: Vec<Value>) -> Result<Value, NsError> {
        let wire = self.wire.as_ref().ok_or_else(|| {
            NsError::Transport(
                "desk.* is not connected — the pack is assembled without a wire (config-level plug)"
                    .into(),
            )
        })?;
        let params = assemble(self.method, &args)?;
        // The declared method name IS the wire op (one vocabulary).
        let answer = wire.request(self.method, Value::Object(params)).await?;
        finish(self.method, answer)
    }
}

/// The `desk.*` capability pack: one declared face over one wire channel.
pub struct PhiNsDesktop {
    wire: Option<Arc<dyn Transport>>,
}

impl PhiNsDesktop {
    /// Connected pack: every call goes out over `wire` to the cua stub.
    pub fn new(wire: Arc<dyn Transport>) -> Self {
        Self { wire: Some(wire) }
    }

    /// Declaration-only pack (docs, contract tests): calls fail with
    /// "not connected" instead of hanging or inventing results.
    pub fn unconnected() -> Self {
        Self { wire: None }
    }
}

#[async_trait]
impl NamespaceProvider for PhiNsDesktop {
    fn namespaces(&self) -> &'static [&'static str] {
        &["desk"]
    }

    fn methods(&self) -> Vec<MethodSpec> {
        methods::method_table()
    }

    async fn install(&self, sink: &mut dyn WorkerSink) -> Result<(), NsError> {
        for spec in self.methods() {
            sink.register_fn(
                spec.name,
                Box::new(DCall {
                    wire: self.wire.clone(),
                    method: spec.name,
                }),
            )?;
        }
        sink.eval(PY_SHIM).await
    }

    fn summarize(&self, _name: &str, obj: &NsObject) -> Option<String> {
        if matches!(
            obj.kind.as_str(),
            "DeskNs" | "WinState" | "El" | "ImageHandle"
        ) {
            Some(obj.detail.clone().unwrap_or_else(|| obj.repr.clone()))
        } else {
            None
        }
    }
}

/// Named wire params for one method — exposed for the pack's tests to pin
/// the mapping without a live stub.
pub fn debug_assemble(method: &str, args: &[Value]) -> Result<Value, NsError> {
    Ok(Value::Object(assemble(method, args)?))
}

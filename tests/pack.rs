//! The `desk.*` declaration source and its wire mapping (O10 inventory §1).
//!
//! The desktop pack is thin by design: named params out to the cua stub under
//! the declared method name (one vocabulary — the stub is ours), answers
//! straight back. What the tests pin is the face itself (21 methods, the
//! inventory's decision columns) and the name → param mapping the stub
//! decodes against.

use async_trait::async_trait;
use phi_cap::{Capability, CapabilitySpec};
use phi_ns::remote::Transport;
use phi_ns::{HostCall, MethodSpec, NamespaceProvider, NsError, NsObject, WorkerSink};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;

phi_ns::provider_contract_tests!(phi_ns_desktop::PhiNsDesktop::unconnected());

// ---- fake sink + wire, to drive the real install() → HostCall path ----

#[derive(Default)]
struct FakeSink {
    fns: BTreeMap<String, Box<dyn HostCall>>,
}

#[async_trait]
impl WorkerSink for FakeSink {
    fn register_fn(&mut self, name: &str, call: Box<dyn HostCall>) -> Result<(), NsError> {
        self.fns.insert(name.to_string(), call);
        Ok(())
    }

    async fn eval(&mut self, _code: &str) -> Result<(), NsError> {
        Ok(())
    }

    async fn namespace_entries(&mut self) -> Result<Vec<NsObject>, NsError> {
        Ok(Vec::new())
    }
}

impl FakeSink {
    async fn invoke(&self, method: &str, args: Vec<Value>) -> Result<Value, NsError> {
        self.fns
            .get(method)
            .unwrap_or_else(|| panic!("{method} not installed"))
            .call(args)
            .await
    }
}

#[derive(Default)]
struct MockWire {
    seen: Mutex<Vec<(String, Value)>>,
    reply: Mutex<Value>,
}

impl MockWire {
    fn answering(reply: Value) -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            reply: Mutex::new(reply),
        }
    }

    fn last(&self) -> (String, Value) {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("no wire request was sent")
    }
}

#[async_trait]
impl Transport for MockWire {
    async fn request(&self, method: &str, args: Value) -> Result<Value, NsError> {
        self.seen.lock().unwrap().push((method.to_string(), args));
        Ok(self.reply.lock().unwrap().clone())
    }
}

async fn installed(reply: Value) -> (FakeSink, std::sync::Arc<MockWire>) {
    let wire = std::sync::Arc::new(MockWire::answering(reply));
    let pack = phi_ns_desktop::PhiNsDesktop::new(wire.clone());
    let mut sink = FakeSink::default();
    pack.install(&mut sink).await.expect("install");
    (sink, wire)
}

// ---- declaration shape ----

fn table() -> Vec<MethodSpec> {
    phi_ns_desktop::PhiNsDesktop::unconnected().methods()
}

#[test]
fn the_declaration_source_is_the_21_method_face() {
    let methods = table();
    assert_eq!(methods.len(), 21, "the desk.* face is 21 methods");
    for m in &methods {
        assert!(
            m.name.starts_with("desk."),
            "{} is not rooted at desk",
            m.name
        );
        assert!(!m.doc.trim().is_empty(), "{} has no doc", m.name);
    }
    let multimodal: Vec<&str> = methods
        .iter()
        .filter(|m| m.multimodal)
        .map(|m| m.name)
        .collect();
    assert_eq!(multimodal, ["desk.shot"], "only desk.shot returns pictures");
    let doc = phi_ns_desktop::PhiNsDesktop::unconnected().api_doc();
    assert_eq!(doc.lines().count(), 21, "one seed line per method:\n{doc}");
}

#[test]
fn the_inventory_decision_columns_partition_the_face() {
    // Inventory §5, the columns MethodPolicy's initial rows are generated
    // from. The pack owns the names, policy owns the labels; this pins the
    // names each label was drawn for.
    const FLOW: &[&str] = &[
        "desk.shot",
        "desk.win_state",
        "desk.windows",
        "desk.apps",
        "desk.screen",
        "desk.regions",
        "desk.clip_read",
        "desk.verify",
        "desk.session",
    ];
    const ASK: &[&str] = &[
        "desk.click",
        "desk.drag",
        "desk.move",
        "desk.scroll",
        "desk.key",
        "desk.hotkey",
        "desk.type",
        "desk.menu",
        "desk.clip_write",
        "desk.win_frame",
        "desk.escalate",
    ];
    const ALLOW: &[&str] = &["desk.agent_cursor"];
    let names: Vec<&str> = table().iter().map(|m| m.name).collect();
    for name in FLOW.iter().chain(ASK).chain(ALLOW) {
        assert!(names.contains(name), "{name} dropped out of the face");
    }
    assert_eq!(FLOW.len() + ASK.len() + ALLOW.len(), names.len());
}

#[test]
fn the_clipboard_split_follows_the_decision_boundary() {
    // Reading the clipboard flows; writing it asks. One resource, two rows,
    // two decisions — that is why they are two methods.
    let read = table()
        .into_iter()
        .find(|m| m.name == "desk.clip_read")
        .unwrap();
    assert!(
        read.caps
            .iter()
            .all(|c| matches!(c, CapabilitySpec::DeviceCapture(_))),
        "clip_read is a capture"
    );
    let write = table()
        .into_iter()
        .find(|m| m.name == "desk.clip_write")
        .unwrap();
    assert!(
        write
            .caps
            .iter()
            .all(|c| matches!(c, CapabilitySpec::DeviceInput(_))),
        "clip_write is an input"
    );
}

#[test]
fn agent_cursor_is_the_lone_allow_with_an_input_envelope() {
    // Caps declare the resource envelope; MethodPolicy decides. agent_cursor
    // moves a visible hint on the desktop (input envelope) but nobody has to
    // approve a cursor wiggle (Allow) — the one deliberate disagreement.
    let m = table()
        .into_iter()
        .find(|m| m.name == "desk.agent_cursor")
        .unwrap();
    assert!(
        m.caps
            .iter()
            .any(|c| matches!(c, CapabilitySpec::DeviceInput(_))),
        "the envelope is still device input"
    );
    assert_eq!(m.capability(), Capability::Device);
}

// ---- name → param mapping (what the cua stub decodes) ----

#[test]
fn named_params_assemble_positional_args() {
    let click =
        phi_ns_desktop::debug_assemble("desk.click", &[json!({"token": "el-7"}), json!("left")])
            .unwrap();
    assert_eq!(click["target"]["token"], "el-7");
    assert_eq!(click["button"], "left");

    // A short call pads the tail with nulls rather than shifting names.
    let scroll = phi_ns_desktop::debug_assemble("desk.scroll", &[json!("down")]).unwrap();
    assert_eq!(scroll["direction"], "down");
    assert!(scroll["amount"].is_null());
    assert!(scroll["target"].is_null());

    let frame = phi_ns_desktop::debug_assemble(
        "desk.win_frame",
        &[json!("win-1"), json!(10), json!(20), json!(800), json!(600)],
    )
    .unwrap();
    assert_eq!(frame["window"], "win-1");
    assert_eq!(frame["w"], 800);
    assert_eq!(frame["h"], 600);
}

#[test]
fn undeclared_methods_never_assemble() {
    let err = phi_ns_desktop::debug_assemble("desk.steal_cookies", &[]).unwrap_err();
    assert!(
        matches!(err, NsError::UndeclaredMethod(_)),
        "expected UndeclaredMethod, got {err:?}"
    );
}

// ---- end-to-end through install() → HostCall → wire ----

#[tokio::test]
async fn the_wire_op_is_the_declared_method_name() {
    let (sink, wire) = installed(json!({"ok": true})).await;
    sink.invoke("desk.click", vec![json!({"token": "el-7"}), json!("left")])
        .await
        .expect("click");
    let (op, params) = wire.last();
    assert_eq!(
        op, "desk.click",
        "one vocabulary: the method name IS the op"
    );
    assert_eq!(params["target"]["token"], "el-7");
    assert_eq!(params["button"], "left");
}

#[tokio::test]
async fn an_unconnected_pack_fails_instead_of_hanging() {
    let pack = phi_ns_desktop::PhiNsDesktop::unconnected();
    let mut sink = FakeSink::default();
    pack.install(&mut sink).await.expect("install");
    let err = sink
        .invoke("desk.windows", vec![])
        .await
        .expect_err("no wire, no answer");
    assert!(
        matches!(err, NsError::Transport(_)),
        "expected Transport, got {err:?}"
    );
}

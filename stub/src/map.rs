//! `desk.*` ⇄ cua-driver tool mapping — the stub's whole vocabulary brain.
//!
//! Pure functions over JSON plus a call callback, so every mapping is unit-
//! testable without a device (live-fumbles-into-tests: the mapping is where
//! the wire lies). Two contracts meet here:
//!
//! * **in**: the pack's named params (`phi-ns-desktop` `assemble()` output);
//! * **out**: the shapes `phi-ns-desktop`'s Python shim consumes
//!   (`ImageHandle` / `WinState` / `El`).
//!
//! Capture roles come from the pack's declaration table (single source): a
//! `Mint` look stores cua's native `capture_id`, a coordinate `Consume` act
//! spends it — the same one-use discipline the host ledger keeps on our side.

use phi_wire_protocol::Error;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One MCP round-trip the mapping needs (the driver child in production, a
/// scripted double in tests). `tool` is a cua tool name.
pub type Call<'a> = &'a mut dyn FnMut(&str, Value) -> Result<Value, Error>;

/// Stub-side state: where captures land on disk and cua's native capture ids.
pub struct State {
    /// Directory screenshots are written to (`screenshot_out_file`).
    pub shots: PathBuf,
    /// Native capture id from the last desktop-scope look.
    pub desktop_capture: Option<String>,
    /// Native capture id from the last window-scope look (window key, id).
    pub window_capture: Option<(String, String)>,
    /// The last desktop look's **published** picture size (the PNG the model
    /// actually sees). The driver may hand back a downscaled desktop capture
    /// (its `max_image_dimension` cap) while desktop-scope coordinates are
    /// still read in that published picture's pixels — so a screen-absolute
    /// aim point has to be computed from these, not from the point size.
    desktop_picture: Option<(f64, f64)>,
    /// Shot file path → the native capture id the picture was cut from.
    native_by_path: HashMap<String, String>,
    /// Element token → the pid whose window minted it. cua resolves a token
    /// inside one app's element cache and demands that app's pid on the call.
    token_pids: HashMap<String, u64>,
    /// Element token → its centre in the minting look's capture pixels: the
    /// element px rung for roles that cannot AXPress (`PxRung`).
    token_px: HashMap<String, PxRung>,
}

impl State {
    /// Fresh state writing shots into `shots`.
    pub fn new(shots: PathBuf) -> Self {
        Self {
            shots,
            desktop_capture: None,
            window_capture: None,
            desktop_picture: None,
            native_by_path: HashMap::new(),
            token_pids: HashMap::new(),
            token_px: HashMap::new(),
        }
    }
}

/// Handle one `desk.*` request: plan the cua call(s), shape the answer.
pub fn dispatch(
    state: &mut State,
    op: &str,
    params: &Map<String, Value>,
    call: Call<'_>,
) -> Result<Value, Error> {
    match op {
        "desk.shot" => shot(state, params, call),
        "desk.win_state" => win_state(state, params, call),
        "desk.windows" => windows(call),
        "desk.apps" => Ok(call("list_apps", Value::Object(Map::new()))?),
        "desk.screen" => screen(call),
        "desk.regions" => regions(state, params, call),
        "desk.verify" => verify(params, call),
        "desk.click" => click(state, params, call),
        "desk.drag" => drag(params, call),
        "desk.move" => move_cursor(params, call),
        "desk.scroll" => scroll(state, params, call),
        "desk.key" => key(params, call),
        "desk.hotkey" => hotkey(params, call),
        "desk.type" => type_text(params, call),
        "desk.menu" => menu(params, call),
        "desk.clip_read" => clip_read(call),
        "desk.clip_write" => clip_write(params, call),
        "desk.win_frame" => win_frame(params, call),
        "desk.session" => session(params, call),
        "desk.escalate" => escalate(call),
        "desk.agent_cursor" => agent_cursor(params, call),
        other => Err(Error::new(
            "UNSUPPORTED_CAPABILITY",
            format!("the cua stub has no mapping for {other}"),
        )),
    }
}

// ---- lookers: capture the world, bind what was seen ----

fn shot(
    state: &mut State,
    params: &Map<String, Value>,
    mut call: Call<'_>,
) -> Result<Value, Error> {
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("desktop");
    let out = state.shots.join(format!("desk-shot-{}.png", stamp()));
    match mode {
        "desktop" => {
            let mut a = Map::new();
            a.insert(
                "screenshot_out_file".into(),
                json!(out.display().to_string()),
            );
            let r = call("get_desktop_state", Value::Object(a))?;
            let rev = store_capture(state, "desktop", &r, &out);
            let mut handle = image_handle(&r, &out, "the whole desktop");
            if let Some(obj) = handle.as_object_mut() {
                obj.insert("window".into(), Value::Null);
                obj.insert("rotation".into(), json!(0));
                obj.insert("revision".into(), json!(rev));
            }
            Ok(handle)
        }
        "window" => {
            let w = resolve_window(params.get("window"), &mut call)?;
            let mut a = Map::new();
            a.insert("pid".into(), json!(w.pid));
            a.insert("window_id".into(), json!(w.window_id));
            a.insert("include_screenshot".into(), json!(true));
            a.insert(
                "screenshot_out_file".into(),
                json!(out.display().to_string()),
            );
            let r = call("get_window_state", Value::Object(a))?;
            let rev = store_capture(state, &w.key(), &r, &out);
            let mut handle = image_handle(&r, &out, "one window");
            if let Some(obj) = handle.as_object_mut() {
                obj.insert("window".into(), w.to_json());
                obj.insert("rotation".into(), json!(0));
                obj.insert("revision".into(), json!(rev));
            }
            Ok(handle)
        }
        "region" => region_shot(state, params, &out, call),
        other => Err(Error::new(
            "INVALID_ARGUMENT",
            format!("shot mode must be desktop/window/region, got {other}"),
        )),
    }
}

/// Region mode: capture the desktop, crop the rect on disk. The binding still
/// names the full capture the rect was cut from.
fn region_shot(
    state: &mut State,
    params: &Map<String, Value>,
    out: &std::path::Path,
    call: Call<'_>,
) -> Result<Value, Error> {
    let rect = params
        .get("region")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::new(
                "INVALID_ARGUMENT",
                "shot(mode=\"region\") needs region=[x, y, w, h]",
            )
        })?;
    let nums: Vec<i64> = rect
        .iter()
        .map(|v| {
            v.as_i64().ok_or_else(|| {
                Error::new("INVALID_ARGUMENT", "region=[x, y, w, h] must be numbers")
            })
        })
        .collect::<Result<_, _>>()?;
    if nums.len() != 4 {
        return Err(Error::new(
            "INVALID_ARGUMENT",
            "region=[x, y, w, h] must have four numbers",
        ));
    }
    let full = state.shots.join(format!("desk-full-{}.png", stamp()));
    let mut a = Map::new();
    a.insert(
        "screenshot_out_file".into(),
        json!(full.display().to_string()),
    );
    let r = call("get_desktop_state", Value::Object(a))?;
    let rev = store_capture(state, "desktop", &r, &full);
    crop_png(&full, out, nums[0], nums[1], nums[2], nums[3])?;
    let mut handle = image_handle(&r, out, "a screen region");
    if let Some(obj) = handle.as_object_mut() {
        // The model gets the crop, not the full capture it was cut from.
        obj.insert("path".into(), json!(out.display().to_string()));
        obj.insert("window".into(), Value::Null);
        obj.insert("rotation".into(), json!(0));
        obj.insert("width".into(), json!(nums[2]));
        obj.insert("height".into(), json!(nums[3]));
        obj.insert("revision".into(), json!(rev));
    }
    Ok(handle)
}

fn win_state(
    state: &mut State,
    params: &Map<String, Value>,
    mut call: Call<'_>,
) -> Result<Value, Error> {
    let w = resolve_window(params.get("window"), &mut call)?;
    let mut a = Map::new();
    a.insert("pid".into(), json!(w.pid));
    a.insert("window_id".into(), json!(w.window_id));
    a.insert("include_accessibility_tree".into(), json!(true));
    let r = call("get_window_state", Value::Object(a))?;
    let rev = store_capture(state, &w.key(), &r, &PathBuf::new());
    // Remember where each element handle came from: a later token click must
    // name this window's pid (cua's click resolves tokens per-pid), and the
    // element px rung clicks the element's centre in this look's own capture
    // pixels (the picture may be downscaled from the Retina backing store).
    if let Some(rows) = r.get("elements").and_then(Value::as_array) {
        for el in rows {
            if let Some(token) = el.get("element_token").and_then(Value::as_str) {
                state.token_pids.insert(token.to_string(), w.pid);
                if let Some(px) = element_px(&w, el, &r) {
                    state.token_px.insert(token.to_string(), px);
                }
            }
        }
    }
    let els = r
        .get("elements")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|el| {
                    json!({
                        "token": el.get("element_token").cloned().unwrap_or(Value::Null),
                        "role": el.get("role").cloned().unwrap_or(json!("")),
                        "text": el.get("label").cloned()
                            .filter(|v| !v.is_null())
                            .or_else(|| el.get("value").cloned())
                            .unwrap_or(json!("")),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let revision = rev.unwrap_or_else(|| w.key());
    let mut out = json!({
        "window": w.to_json(),
        "revision": revision,
        "els": els,
        "tree": r.get("elements").cloned().unwrap_or(Value::Null),
        "width": r.get("screenshot_width").cloned().unwrap_or(Value::Null),
        "height": r.get("screenshot_height").cloned().unwrap_or(Value::Null),
    });
    // An empty tree is a degradation, not "the window has no controls" — the
    // driver says why (`ax_tree_empty` / `ax_window_unresolved`) and the wire
    // must never swallow that verdict.
    if let Some(obj) = out.as_object_mut() {
        for k in ["degraded", "degraded_reason", "escalation", "snapshot_id"] {
            if let Some(v) = r.get(k) {
                obj.insert(k.into(), v.clone());
            }
        }
    }
    Ok(out)
}

fn windows(call: Call<'_>) -> Result<Value, Error> {
    let r = call("list_windows", Value::Object(Map::new()))?;
    let mut rows = r.get("windows").cloned().unwrap_or(Value::Null);
    // cua's z_index is not a stacking order (see `focused_window`) — re-stamp
    // it to the true front rank so a consumer picking "the front one" lands
    // where the user is looking. No tree, no rewrite: never invent order.
    if let (Some(order), Some(arr)) = (front_order(call).as_ref(), rows.as_array_mut()) {
        let len = order.len() as i64;
        for w in arr.iter_mut() {
            let Some(id) = w.get("window_id").and_then(Value::as_u64) else {
                continue;
            };
            // Off the tree (off-screen, other space): visible windows always
            // outrank it, so its rank is the floor.
            let rank = order.get(&id).map(|&idx| len - idx as i64).unwrap_or(0);
            if let Some(obj) = w.as_object_mut() {
                obj.insert("z_index".into(), json!(rank));
            }
        }
    }
    let lines: Vec<String> = rows
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|w| {
                    format!(
                        "[{}] {} {}",
                        w.get("window_id").and_then(Value::as_u64).unwrap_or(0),
                        w.get("app_name").and_then(Value::as_str).unwrap_or(""),
                        w.get("title").and_then(Value::as_str).unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({ "windows": rows, "lines": lines }))
}

fn screen(call: Call<'_>) -> Result<Value, Error> {
    let size = call("get_screen_size", Value::Object(Map::new()))?;
    let pointer = call("get_cursor_position", Value::Object(Map::new()))?;
    Ok(json!({
        "width": size.get("width").cloned().unwrap_or(Value::Null),
        "height": size.get("height").cloned().unwrap_or(Value::Null),
        "scale_factor": size.get("scale_factor").cloned().unwrap_or(Value::Null),
        "pointer": pointer,
    }))
}

fn regions(state: &mut State, params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    // The parse registry is cua's own, keyed by its native capture ids — the
    // host's minted id on the shot handle means nothing here. Follow the shot
    // handle's picture back to the capture it was cut from; with no handle
    // (the seed default), parse the most recent look.
    let capture = params
        .get("shot")
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .and_then(|p| state.native_by_path.get(p).cloned())
        .or_else(|| state.desktop_capture.clone())
        .or_else(|| state.window_capture.as_ref().map(|(_, id)| id.clone()))
        .ok_or_else(|| {
            Error::new(
                "STALE_TARGET",
                "no capture to parse — look (desk.shot) before desk.regions",
            )
        })?;
    let mut a = Map::new();
    a.insert("capture_id".into(), json!(capture));
    call("parse_visual_regions", Value::Object(a))
}

fn verify(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    // `claim` is the predicates verbatim: a list, or `{"expect": [...]}`.
    let claim = params.get("claim").cloned().unwrap_or(Value::Null);
    let expect = match claim {
        Value::Array(rows) => Value::Array(rows),
        Value::Object(obj) => obj
            .get("expect")
            .cloned()
            .ok_or_else(|| Error::new("INVALID_ARGUMENT", "verify claim needs an expect list"))?,
        other => {
            return Err(Error::new(
                "INVALID_ARGUMENT",
                format!("verify claim must be predicates, got {other}"),
            ));
        }
    };
    let mut a = Map::new();
    a.insert("expect".into(), expect);
    call("verify_state", Value::Object(a))
}

// ---- acts: one target, one attempt ----

fn click(state: &State, params: &Map<String, Value>, mut call: Call<'_>) -> Result<Value, Error> {
    let button = params
        .get("button")
        .and_then(Value::as_str)
        .map(str::to_string);
    match target_fields(params.get("target"))? {
        TargetFields::Token { token } => token_click(state, &token, button.as_deref(), &mut call),
        TargetFields::Point { x, y } => {
            let mut a = Map::new();
            a.insert("x".into(), json!(x));
            a.insert("y".into(), json!(y));
            // Windowless screen pixels are desktop scope; without the flag cua
            // reads x,y as window-local and refuses (`desktop_scope_disabled`).
            a.insert("scope".into(), json!("desktop"));
            // Coordinate acts ride the look's native capture (cua's own
            // one-use rule); element tokens are already snapshot-bound.
            if let Some(cap) = state
                .desktop_capture
                .clone()
                .or_else(|| state.window_capture.as_ref().map(|(_, id)| id.clone()))
            {
                a.insert("capture_id".into(), json!(cap));
            }
            if let Some(button) = button.as_deref() {
                a.insert("button".into(), json!(button));
            }
            act(call("click", Value::Object(a))?)
        }
    }
}

/// One element act: the semantic AX click first (cua resolves the token in
/// the minting pid's element cache), and — only when the driver answers that
/// this role has no AXPress to perform — the element px rung: a pixel click
/// at the element's centre in the minting capture's pixels, spending that
/// capture. One wire act either way; the rungs are delivery mechanics (cua's
/// own click ladder is background AX → pixel → foreground and only rescues
/// collection rows itself, so the px rung belongs to this translator).
fn token_click(
    state: &State,
    token: &str,
    button: Option<&str>,
    call: Call<'_>,
) -> Result<Value, Error> {
    let pid = state.token_pids.get(token).copied().ok_or_else(|| {
        Error::new(
            "STALE_TARGET",
            format!("no window owns element token {token} — look (desk.win_state) again"),
        )
    })?;
    let mut a = Map::new();
    a.insert("element_token".into(), json!(token));
    a.insert("pid".into(), json!(pid));
    if let Some(button) = button {
        a.insert("button".into(), json!(button));
    }
    match call("click", Value::Object(a)) {
        Err(e) if ax_press_unsupported(&e.message) => {
            // Nothing was applied: the action is unsupported, not refused
            // mid-flight. The intent may still land on the pixel rung.
            let Some(px) = state.token_px.get(token) else {
                return Err(e);
            };
            if px.x < 0.0 || px.y < 0.0 || px.x > px.png_w || px.y > px.png_h {
                // The centre is off the picture the look proved — there is no
                // honest pixel to click, and guessing one is a misclick.
                return Err(e);
            }
            let mut a = Map::new();
            a.insert("pid".into(), json!(pid));
            a.insert("window_id".into(), json!(px.window_id));
            a.insert("x".into(), json!(px.x));
            a.insert("y".into(), json!(px.y));
            a.insert("capture_id".into(), json!(px.capture));
            if let Some(button) = button {
                a.insert("button".into(), json!(button));
            }
            act(call("click", Value::Object(a))?)
        }
        other => act(other?),
    }
}

/// The one driver failure whose intent is still deliverable on another rung:
/// `AXUIElementPerformAction(AXPress)` answered -25206
/// (`kAXErrorActionUnsupported`) — the role has no press action (a text view
/// takes a caret, not a press) and nothing was applied. Any other failure is
/// an outcome, not a mechanism gap, and comes back as classified.
fn ax_press_unsupported(msg: &str) -> bool {
    msg.contains("AXUIElementPerformAction") && msg.contains("-25206")
}

/// The element px rung of cua's click ladder: where one element sits inside
/// the capture its look actually wrote. `x`/`y` are that picture's pixels
/// (top-left origin) — the coordinates a `click` with `capture_id` admits —
/// and `capture` is the native id the look bound, so the rung spends the
/// same one-use capture the wire books for this act.
#[derive(Debug, Clone)]
struct PxRung {
    window_id: u64,
    capture: String,
    x: f64,
    y: f64,
    png_w: f64,
    png_h: f64,
}

/// Where one element row sits in the minting look's capture pixels — only
/// when the look proved a frame (`window_bounds` against a validated
/// screenshot). The element `frame` is in screen points; the picture is the
/// look's own (possibly downscaled) PNG, so scale points into its pixels.
fn element_px(w: &WinRef, el: &Value, r: &Value) -> Option<PxRung> {
    let capture = r.get("capture_id").and_then(Value::as_str)?;
    if r.get("screenshot_frame_valid").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let frame = el.get("frame")?;
    let (fx, fy, fw, fh) = (
        frame.get("x").and_then(Value::as_f64)?,
        frame.get("y").and_then(Value::as_f64)?,
        frame.get("w").and_then(Value::as_f64)?,
        frame.get("h").and_then(Value::as_f64)?,
    );
    let b = r.get("window_bounds")?;
    let (bx, by, bw, bh) = (
        b.get("x").and_then(Value::as_f64)?,
        b.get("y").and_then(Value::as_f64)?,
        b.get("width").and_then(Value::as_f64)?,
        b.get("height").and_then(Value::as_f64)?,
    );
    if bw <= 0.0 || bh <= 0.0 {
        return None;
    }
    let png_w = r.get("screenshot_width").and_then(Value::as_f64)?;
    let png_h = r.get("screenshot_height").and_then(Value::as_f64)?;
    Some(PxRung {
        window_id: w.window_id,
        capture: capture.to_string(),
        x: (fx + fw / 2.0 - bx) * (png_w / bw),
        y: (fy + fh / 2.0 - by) * (png_h / bh),
        png_w,
        png_h,
    })
}

fn drag(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let (fx, fy) = point(params.get("start"))?;
    let (tx, ty) = point(params.get("end"))?;
    let mut a = Map::new();
    // The wire carries desktop coordinates; scope says so or cua demands a pid.
    a.insert("scope".into(), json!("desktop"));
    a.insert("from_x".into(), json!(fx));
    a.insert("from_y".into(), json!(fy));
    a.insert("to_x".into(), json!(tx));
    a.insert("to_y".into(), json!(ty));
    if let Some(ms) = params.get("duration_ms").and_then(Value::as_f64) {
        a.insert("duration_ms".into(), json!(ms as i64));
    }
    act(call("drag", Value::Object(a))?)
}

fn move_cursor(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let (x, y) = point(params.get("target"))?;
    let mut a = Map::new();
    a.insert("x".into(), json!(x));
    a.insert("y".into(), json!(y));
    // Desktop scope, same as click/drag/scroll: without it cua moves only
    // the agent's overlay cursor and the real OS pointer never moves.
    a.insert("scope".into(), json!("desktop"));
    act(call("move_cursor", Value::Object(a))?)
}

fn scroll(state: &State, params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let (x, y) = match params.get("target") {
        Some(t) if !t.is_null() => point(Some(t))?,
        // Viewport scrolling has no element under it: aim at the screen
        // centre — but in the space the driver reads desktop coordinates in.
        // After a desktop look that space is the *published* picture's pixels
        // (the driver scales them back to native); with no desktop look there
        // is no cap to undo, so the space is native pixels.
        _ => match state.desktop_picture {
            Some((w, h)) => (w / 2.0, h / 2.0),
            None => {
                let size = call("get_screen_size", Value::Object(Map::new()))?;
                let scale = size
                    .get("scale_factor")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0);
                (
                    size.get("width").and_then(Value::as_f64).unwrap_or(0.0) / 2.0 * scale,
                    size.get("height").and_then(Value::as_f64).unwrap_or(0.0) / 2.0 * scale,
                )
            }
        },
    };
    let mut a = Map::new();
    a.insert("x".into(), json!(x));
    a.insert("y".into(), json!(y));
    // Screen-absolute scroll without a window: desktop scope, same as click.
    a.insert("scope".into(), json!("desktop"));
    let direction = params
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("down");
    a.insert("direction".into(), json!(direction));
    // The wire's amount is pixels-ish (seed default 400); cua counts lines.
    let amount = params
        .get("amount")
        .and_then(Value::as_f64)
        .unwrap_or(400.0);
    a.insert(
        "amount".into(),
        json!((amount / 40.0).round().clamp(1.0, 50.0) as i64),
    );
    a.insert("by".into(), json!("line"));
    act(call("scroll", Value::Object(a))?)
}

fn key(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let mut a = Map::new();
    // Keystrokes are blind input: desktop scope sends them frontmost.
    a.insert("scope".into(), json!("desktop"));
    a.insert(
        "key".into(),
        params.get("key").cloned().unwrap_or(Value::Null),
    );
    act(call("press_key", Value::Object(a))?)
}

fn hotkey(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let mut a = Map::new();
    a.insert("scope".into(), json!("desktop"));
    a.insert(
        "keys".into(),
        params.get("keys").cloned().unwrap_or(Value::Null),
    );
    act(call("hotkey", Value::Object(a))?)
}

fn type_text(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let mut a = Map::new();
    a.insert("scope".into(), json!("desktop"));
    a.insert(
        "text".into(),
        params.get("text").cloned().unwrap_or(Value::Null),
    );
    act(call("type_text", Value::Object(a))?)
}

fn menu(params: &Map<String, Value>, mut call: Call<'_>) -> Result<Value, Error> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("INVALID_ARGUMENT", "menu needs a path like \"File > Save\""))?;
    let segments: Vec<Value> = path
        .split('>')
        .map(|s| json!(s.trim()))
        .filter(|s| s.as_str() != Some(""))
        .collect();
    if segments.is_empty() {
        return Err(Error::new("INVALID_ARGUMENT", "menu path is empty"));
    }
    // Menu invocation is per-window; the focused window is the one whose menu
    // bar the user sees.
    let w = focused_window(&mut call)?;
    let mut a = Map::new();
    a.insert("pid".into(), json!(w.pid));
    a.insert("window_id".into(), json!(w.window_id));
    a.insert("path".into(), Value::Array(segments));
    act(call("invoke_menu", Value::Object(a))?)
}

fn clip_read(call: Call<'_>) -> Result<Value, Error> {
    let mut a = Map::new();
    a.insert("include_text".into(), json!(true));
    call("clipboard_read", Value::Object(a))
}

fn clip_write(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let value = params.get("value").cloned().unwrap_or(Value::Null);
    let mut a = Map::new();
    match value.as_str() {
        Some(text) => {
            let looks_like_image = std::path::Path::new(text)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "png" | "jpg" | "jpeg" | "gif" | "webp"));
            if looks_like_image {
                a.insert("image_path".into(), json!(text));
            } else {
                a.insert("text".into(), json!(text));
            }
        }
        None => {
            a.insert("text".into(), value);
        }
    }
    act(call("clipboard_write", Value::Object(a))?)
}

fn win_frame(params: &Map<String, Value>, mut call: Call<'_>) -> Result<Value, Error> {
    let w = resolve_window(params.get("window"), &mut call)?;
    let mut a = Map::new();
    a.insert("pid".into(), json!(w.pid));
    a.insert("window_id".into(), json!(w.window_id));
    for (wire, cua) in [("x", "x"), ("y", "y"), ("w", "width"), ("h", "height")] {
        if let Some(v) = params.get(wire) {
            a.insert(cua.into(), v.clone());
        }
    }
    act(call("set_window_frame", Value::Object(a))?)
}

fn session(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let op = params.get("op").and_then(Value::as_str).unwrap_or("get");
    match op {
        "get" => call("get_session", Value::Object(Map::new())),
        "list" => call("list_sessions", Value::Object(Map::new())),
        "state" => call("get_session_state", Value::Object(Map::new())),
        other => Err(Error::new(
            "INVALID_ARGUMENT",
            format!("session op must be get/list/state, got {other}"),
        )),
    }
}

fn escalate(call: Call<'_>) -> Result<Value, Error> {
    let mut a = Map::new();
    // The model's escalate() carries no reason; "other" is the honest default
    // and the approval already happened at the host Gate.
    a.insert("reason".into(), json!("other"));
    call("escalate_session", Value::Object(a))
}

fn agent_cursor(params: &Map<String, Value>, call: Call<'_>) -> Result<Value, Error> {
    let op = params.get("op").and_then(Value::as_str).unwrap_or("state");
    let fields = params
        .get("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    match op {
        "state" => call("get_agent_cursor_state", Value::Object(Map::new())),
        "move" => {
            let mut a = Map::new();
            for (k, v) in &fields {
                a.insert(k.clone(), v.clone());
            }
            call("set_agent_cursor_motion", Value::Object(a))
        }
        "set" => {
            // One verb, possibly two cua tools: visibility and style.
            let mut out = Map::new();
            if fields.contains_key("enabled") {
                let mut a = Map::new();
                a.insert("enabled".into(), fields["enabled"].clone());
                out.extend(as_obj(call("set_agent_cursor_enabled", Value::Object(a))?));
            }
            if fields.contains_key("theme_id") {
                let mut a = Map::new();
                a.insert("theme_id".into(), fields["theme_id"].clone());
                if let Some(rm) = fields.get("reduced_motion") {
                    a.insert("reduced_motion".into(), rm.clone());
                }
                out.extend(as_obj(call("set_agent_cursor_theme", Value::Object(a))?));
            }
            if out.is_empty() {
                return Err(Error::new(
                    "INVALID_ARGUMENT",
                    "agent_cursor(op=\"set\") needs enabled or theme_id",
                ));
            }
            Ok(Value::Object(out))
        }
        other => Err(Error::new(
            "INVALID_ARGUMENT",
            format!("agent_cursor op must be state/move/set, got {other}"),
        )),
    }
}

// ---- shared shaping ----

/// Whether a deadline on this call could have left something applied. A look
/// or a read answers "nothing happened, retry"; an act answers "outcome
/// unknown" (inventory §3.3 — never conflate the two).
pub fn mutates(op: &str, params: &Map<String, Value>) -> bool {
    match op {
        "desk.shot" | "desk.win_state" | "desk.windows" | "desk.apps" | "desk.screen"
        | "desk.regions" | "desk.verify" | "desk.clip_read" | "desk.session" => false,
        // Only the agent-cursor reads are reads; its two setters touch state.
        "desk.agent_cursor" => params
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(|o| o != "state"),
        _ => true,
    }
}

/// The wire answer to "the call never came back".
pub fn deadline_error(op: &str, params: &Map<String, Value>) -> Error {
    if mutates(op, params) {
        Error::new(
            "ACTION_OUTCOME_UNKNOWN",
            format!(
                "deadline passed with no reply on {op} — the action may or may not have landed"
            ),
        )
    } else {
        Error::new(
            "ACTION_REJECTED",
            format!("deadline passed with no reply on {op} — nothing was applied"),
        )
    }
}

/// cua act results carry an `effect` verdict (confirmed / partial /
/// unverifiable / suspected_noop / refused). The wire splits on *delivery*,
/// not evidence strength: `refused` alone means nothing was applied and the
/// same act may be retried. Every other verdict was delivered and comes back
/// as a result the model reads, `effect` verbatim — `unverifiable` says the
/// driver moved the real desktop and just cannot see the outcome. An act whose
/// fate is truly unknown produces no verdict at all; that answer belongs to
/// the deadline path (`deadline_error`).
fn act(result: Value) -> Result<Value, Error> {
    let effect = result.get("effect").and_then(Value::as_str).unwrap_or("");
    if effect == "refused" {
        return Err(Error::new(
            "ACTION_REJECTED",
            format!("the driver refused the action: {result}"),
        ));
    }
    Ok(result)
}

/// One window address cua speaks: `pid` + `window_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinRef {
    /// Process id of the window's app.
    pub pid: u64,
    /// Platform window id.
    pub window_id: u64,
}

impl WinRef {
    /// Stable key for capture bookkeeping.
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.window_id)
    }

    fn to_json(&self) -> Value {
        json!({ "pid": self.pid, "window_id": self.window_id })
    }
}

/// What a target turned into after normalization.
enum TargetFields {
    /// An element token from `desk.win_state`.
    Token { token: String },
    /// A point in desktop coordinates.
    Point { x: f64, y: f64 },
}

fn target_fields(target: Option<&Value>) -> Result<TargetFields, Error> {
    let target = target.cloned().unwrap_or(Value::Null);
    if let Some(token) = target
        .get("token")
        .and_then(Value::as_str)
        .or_else(|| target.as_str())
    {
        return Ok(TargetFields::Token {
            token: token.to_string(),
        });
    }
    point(Some(&target)).map(|(x, y)| TargetFields::Point { x, y })
}

fn point(v: Option<&Value>) -> Result<(f64, f64), Error> {
    let v = v.cloned().unwrap_or(Value::Null);
    if let Some(pair) = v.as_array()
        && pair.len() == 2
    {
        let x = pair[0]
            .as_f64()
            .ok_or_else(|| Error::new("INVALID_ARGUMENT", "point x must be a number"))?;
        let y = pair[1]
            .as_f64()
            .ok_or_else(|| Error::new("INVALID_ARGUMENT", "point y must be a number"))?;
        return Ok((x, y));
    }
    if let (Some(x), Some(y)) = (
        v.get("x").and_then(Value::as_f64),
        v.get("y").and_then(Value::as_f64),
    ) {
        return Ok((x, y));
    }
    Err(Error::new(
        "INVALID_ARGUMENT",
        format!("point must be [x, y] or {{x, y}}, got {v}"),
    ))
}

/// Resolve the wire's `window` slot: null is the focused window, a number is a
/// window id, `{"pid", "window_id"}` is exact.
fn resolve_window(window: Option<&Value>, mut call: Call<'_>) -> Result<WinRef, Error> {
    let window = window.cloned().unwrap_or(Value::Null);
    if window.is_null() {
        return focused_window(&mut call);
    }
    if let Some(obj) = window.as_object()
        && let (Some(pid), Some(id)) = (
            obj.get("pid").and_then(Value::as_u64),
            obj.get("window_id").and_then(Value::as_u64),
        )
    {
        return Ok(WinRef { pid, window_id: id });
    }
    let id = window
        .as_u64()
        .or_else(|| window.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| {
            Error::new(
                "INVALID_ARGUMENT",
                format!("window must be null, an id, or {{pid, window_id}}, got {window}"),
            )
        })?;
    let r = call("list_windows", Value::Object(Map::new()))?;
    let rows = r
        .get("windows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .find(|w| w.get("window_id").and_then(Value::as_u64) == Some(id))
        .map(|w| WinRef {
            pid: w.get("pid").and_then(Value::as_u64).unwrap_or(0),
            window_id: id,
        })
        .ok_or_else(|| Error::new("TARGET_NOT_FOUND", format!("no window with id {id}")))
}

/// The window the user is looking at: the active app's front-most window.
/// cua's `z_index` is **not** a stacking order — it is derived from the
/// all-windows WindowServer list, whose order scrambles once off-screen
/// windows are in the mix (live fumble: a leftover TextEdit document outranked
/// the fresh frontmost one, so blind typing appended into the wrong
/// document). The true front-to-back order is `get_accessibility_tree`'s
/// window array: on-screen windows only, index 0 closest to the user.
/// Rank lexicographically — (active app, in the tree, on screen, front score)
/// — where the score is the tree position for tree windows and cua's z for
/// the rest, so z only orders windows the tree never mentions. No tree, no
/// rewrite: a missing answer degrades to the z ranking instead of failing.
fn focused_window(call: Call<'_>) -> Result<WinRef, Error> {
    let apps = call("list_apps", Value::Object(Map::new()))?;
    let active_pid = apps
        .get("apps")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|a| a.get("active").and_then(Value::as_bool) == Some(true))
        })
        .and_then(|a| a.get("pid").and_then(Value::as_u64));
    let r = call("list_windows", Value::Object(Map::new()))?;
    let rows = r
        .get("windows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let order = front_order(call);
    let tree_len = order.as_ref().map_or(0, |o| o.len()) as i64;
    let mut best: Option<((i64, i64, i64, i64), WinRef)> = None;
    for w in &rows {
        let pid = w.get("pid").and_then(Value::as_u64).unwrap_or(0);
        let Some(window_id) = w.get("window_id").and_then(Value::as_u64) else {
            continue;
        };
        if w.get("minimized").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // Missing z counts as backmost — an unranked window never steals the pick.
        let z = w.get("z_index").and_then(Value::as_i64).unwrap_or(i64::MIN);
        let idx = order.as_ref().and_then(|o| o.get(&window_id).copied());
        // Higher wins: (active app, in tree, on screen, front score).
        let rank = (
            i64::from(active_pid == Some(pid)),
            i64::from(idx.is_some()),
            i64::from(w.get("is_on_screen").and_then(Value::as_bool) != Some(false)),
            match idx {
                // Tree index 0 is closest to the user — flip it so higher is front.
                Some(i) => tree_len - i as i64,
                None => z,
            },
        );
        if best.as_ref().is_none_or(|(b, _)| rank > *b) {
            best = Some((rank, WinRef { pid, window_id }));
        }
    }
    best.map(|(_, w)| w)
        .ok_or_else(|| Error::new("TARGET_NOT_FOUND", "no window to act on"))
}

/// Front-to-back window order from `get_accessibility_tree`
/// (window_id → index, 0 = closest to the user). That tree lists only
/// on-screen windows and is the one driver read whose order matches the
/// stacking the user sees. `None` when the driver cannot answer.
fn front_order(call: Call<'_>) -> Option<HashMap<u64, usize>> {
    let r = call("get_accessibility_tree", Value::Object(Map::new())).ok()?;
    Some(
        r.get("windows")?
            .as_array()?
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.get("window_id").and_then(Value::as_u64).map(|id| (id, i)))
            .collect(),
    )
}

fn image_handle(r: &Value, out: &std::path::Path, alt: &str) -> Value {
    let path = r
        .get("screenshot_file_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| out.display().to_string());
    json!({
        "path": path,
        "mime": r.get("screenshot_mime_type").cloned().unwrap_or(json!("image/png")),
        "alt": alt,
        "width": r.get("screenshot_width").cloned()
            .or_else(|| r.get("screen_width").cloned())
            .unwrap_or(Value::Null),
        "height": r.get("screenshot_height").cloned()
            .or_else(|| r.get("screen_height").cloned())
            .unwrap_or(Value::Null),
    })
}

/// Remember cua's native capture id so the next coordinate act can spend it.
/// Returns the id the look bound, if the driver reported one.
fn store_capture(state: &mut State, key: &str, r: &Value, path: &Path) -> Option<String> {
    let id = r.get("capture_id").and_then(Value::as_str)?.to_string();
    if key == "desktop" {
        state.desktop_capture = Some(id.clone());
        // The picture the model reads is the driver's *published* one, which
        // may be downscaled below the screen's native pixels. Desktop-scope
        // coordinates are read in that published picture, so keep its size.
        state.desktop_picture = match (
            r.get("screenshot_width").and_then(Value::as_f64),
            r.get("screenshot_height").and_then(Value::as_f64),
        ) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
            _ => None,
        };
    } else {
        state.window_capture = Some((key.to_string(), id.clone()));
    }
    let picture = r
        .get("screenshot_file_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| path.display().to_string());
    if !picture.is_empty() {
        state.native_by_path.insert(picture, id.clone());
    }
    Some(id)
}

fn as_obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn stamp() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!(
        "{ms}-{}",
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Crop `[x, y, w, h]` out of one PNG on disk — the model asked for a piece
/// of the picture, cua only sells whole captures.
fn crop_png(
    src: &std::path::Path,
    dst: &std::path::Path,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
) -> Result<(), Error> {
    let bytes = std::fs::read(src)
        .map_err(|e| Error::new("ACTION_REJECTED", format!("cannot read capture: {e}")))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| Error::new("ACTION_REJECTED", format!("cannot decode capture: {e}")))?;
    // RgbImage's inherent dimensions; crop_imm itself works on the dynamic one.
    let (cw, ch) = {
        let rgb = img.to_rgb8();
        (rgb.width() as i64, rgb.height() as i64)
    };
    let x = x.clamp(0, cw.saturating_sub(1)) as u32;
    let y = y.clamp(0, ch.saturating_sub(1)) as u32;
    let w = w.clamp(1, cw - x as i64) as u32;
    let h = h.clamp(1, ch - y as i64) as u32;
    img.crop_imm(x, y, w, h)
        .save(dst)
        .map_err(|e| Error::new("ACTION_REJECTED", format!("cannot write region: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A scripted driver: records every call, answers from a canned table.
    struct Script {
        calls: Vec<(String, Value)>,
        answers: BTreeMap<String, Value>,
    }

    impl Script {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                answers: BTreeMap::new(),
            }
        }

        fn answer(mut self, tool: &str, result: Value) -> Self {
            self.answers.insert(tool.to_string(), result);
            self
        }

        fn call(&mut self, tool: &str, args: Value) -> Result<Value, Error> {
            self.calls.push((tool.to_string(), args));
            self.answers
                .get(tool)
                .cloned()
                .ok_or_else(|| Error::new("TARGET_NOT_FOUND", format!("no script for {tool}")))
        }

        fn saw(&self, tool: &str) -> &Value {
            &self
                .calls
                .iter()
                .find(|(t, _)| t == tool)
                .unwrap_or_else(|| panic!("{tool} was never called"))
                .1
        }
    }

    fn params(p: &[(&str, Value)]) -> Map<String, Value> {
        p.iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn desktop_answer() -> Value {
        json!({
            "capture_id": "cap_1",
            "screenshot_file_path": "/tmp/cap1.png",
            "screenshot_mime_type": "image/png",
            "screenshot_width": 1440,
            "screenshot_height": 900,
        })
    }

    fn window_answer() -> Value {
        // A 2x-scale window picture: bounds 400x300 points, PNG 800x600
        // pixels. e1 covers screen (150,250)+(100x40) — centre (200,270) →
        // PNG pixel (200,140).
        json!({
            "capture_id": "cap_w",
            "screenshot_frame_valid": true,
            "window_bounds": {"x": 100.0, "y": 200.0, "width": 400.0, "height": 300.0},
            "elements": [
                {"element_token": "e1", "role": "button", "label": "Save", "value": null,
                 "frame": {"x": 150.0, "y": 250.0, "w": 100.0, "h": 40.0}}
            ],
            "screenshot_width": 800,
            "screenshot_height": 600,
        })
    }

    fn focused_apps() -> Value {
        json!({ "apps": [ {"pid": 7, "active": true} ] })
    }

    fn focused_windows() -> Value {
        json!({ "windows": [
            {"window_id": 3, "pid": 7, "is_on_screen": true, "minimized": false, "z_index": 1}
        ] })
    }

    /// The declaration table is the single source: every declared method must
    /// have a mapping, and the mapping must invent none of its own.
    #[test]
    fn mapping_covers_every_declared_method() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new().answer("get_session", json!({"session": "s"}));
        for spec in phi_ns_desktop::methods::method_table() {
            let p = Map::new();
            let outcome = dispatch(&mut state, spec.name, &p, &mut |t, a| script.call(t, a));
            if let Err(e) = outcome {
                assert!(
                    !e.message.contains("no mapping for"),
                    "{} hit the unmapped arm: {e}",
                    spec.name
                );
            }
        }
        let p = Map::new();
        let e = dispatch(&mut state, "desk.nope", &p, &mut |t, a| script.call(t, a)).unwrap_err();
        assert_eq!(e.code, "UNSUPPORTED_CAPABILITY");
        assert!(e.message.contains("no mapping for desk.nope"));
    }

    /// One look binds what it saw; a coordinate act rides that binding and
    /// speaks desktop scope (cua's windowless x,y contract).
    #[test]
    fn shot_binds_the_look_and_click_spends_it() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("get_desktop_state", desktop_answer())
            .answer("click", json!({"effect": "confirmed"}));
        let p = params(&[
            ("mode", json!("desktop")),
            ("window", Value::Null),
            ("region", Value::Null),
        ]);
        let handle = dispatch(&mut state, "desk.shot", &p, &mut |t, a| script.call(t, a)).unwrap();
        assert_eq!(handle["path"], json!("/tmp/cap1.png"));
        assert_eq!(handle["alt"], json!("the whole desktop"));
        assert_eq!(handle["width"], json!(1440));
        assert_eq!(handle["window"], Value::Null);
        assert_eq!(handle["revision"], json!("cap_1"));
        assert_eq!(state.desktop_capture.as_deref(), Some("cap_1"));

        let p = params(&[("target", json!([10, 20])), ("button", json!("left"))]);
        dispatch(&mut state, "desk.click", &p, &mut |t, a| script.call(t, a)).unwrap();
        let args = script.saw("click");
        assert_eq!(args["x"], json!(10.0));
        assert_eq!(args["y"], json!(20.0));
        assert_eq!(args["capture_id"], json!("cap_1"));
        assert_eq!(args["scope"], json!("desktop"));
    }

    /// An element-token act carries the pid of the window that minted the
    /// handle, and never the coordinate fields (cua's click schema is oneOf).
    #[test]
    fn token_click_carries_the_minting_pid() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("get_window_state", window_answer())
            .answer("click", json!({"effect": "confirmed"}));
        let p = params(&[("window", Value::Null)]);
        dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();

        let p = params(&[
            ("target", json!({"token": "e1"})),
            ("button", json!("left")),
        ]);
        dispatch(&mut state, "desk.click", &p, &mut |t, a| script.call(t, a)).unwrap();
        let args = script.saw("click");
        assert_eq!(args["element_token"], json!("e1"));
        assert_eq!(args["pid"], json!(7));
        assert!(args.get("capture_id").is_none());
        assert!(args.get("x").is_none());
    }

    /// A role with no AXPress (a text view takes a caret, not a press) fails
    /// -25206 with nothing applied; the same act then lands on the element
    /// px rung — the element's centre in the minting capture's picture
    /// pixels, spending that capture. One wire act, two delivery rungs.
    #[test]
    fn unsupported_ax_press_falls_back_to_the_element_px_rung() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();

        let mut clicks: Vec<Value> = Vec::new();
        let p = params(&[
            ("target", json!({"token": "e1"})),
            ("button", json!("left")),
        ]);
        let out = dispatch(&mut state, "desk.click", &p, &mut |t, a| {
            if t == "click" {
                clicks.push(a.clone());
                return if clicks.len() == 1 {
                    Err(Error::new(
                        "ACTION_REJECTED",
                        "AX action failed: AXUIElementPerformAction(AXPress) returned -25206",
                    ))
                } else {
                    Ok(json!({"effect": "confirmed"}))
                };
            }
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(out["effect"], json!("confirmed"));
        assert_eq!(clicks.len(), 2);
        // Rung 1 is the semantic click: the token and its minting pid, and
        // never coordinate fields (cua's click schema is oneOf).
        assert_eq!(clicks[0]["element_token"], json!("e1"));
        assert_eq!(clicks[0]["pid"], json!(7));
        assert!(clicks[0].get("x").is_none());
        // Rung 2 is the element px action: the element's centre in the
        // minting capture's picture pixels, spending that capture.
        assert_eq!(clicks[1]["pid"], json!(7));
        assert_eq!(clicks[1]["window_id"], json!(3));
        assert_eq!(clicks[1]["x"], json!(200.0));
        assert_eq!(clicks[1]["y"], json!(140.0));
        assert_eq!(clicks[1]["capture_id"], json!("cap_w"));
        assert_eq!(clicks[1]["button"], json!("left"));
        assert!(clicks[1].get("element_token").is_none());
    }

    /// Any other driver failure is an outcome, not a mechanism gap: the act
    /// comes back as classified and nothing is ever clicked twice.
    #[test]
    fn a_refused_action_is_not_retried_on_the_pixel_rung() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();

        let mut clicks: Vec<Value> = Vec::new();
        let p = params(&[("target", json!({"token": "e1"}))]);
        let e = dispatch(&mut state, "desk.click", &p, &mut |t, a| {
            if t == "click" {
                clicks.push(a.clone());
                return Err(Error::new(
                    "ACTION_REJECTED",
                    "the device refused the action: target hidden",
                ));
            }
            script.call(t, a)
        })
        .unwrap_err();
        assert_eq!(e.code, "ACTION_REJECTED");
        assert_eq!(clicks.len(), 1);
    }

    /// With no proven frame the px rung has no honest pixel to click: the AX
    /// answer stands and the model decides the next rung itself.
    #[test]
    fn without_a_provable_frame_the_ax_error_is_the_answer() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer(
                "get_window_state",
                json!({
                    "capture_id": "cap_w",
                    "elements": [
                        {"element_token": "e1", "role": "textarea", "label": "body"}
                    ],
                }),
            );
        let p = params(&[("window", Value::Null)]);
        dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();

        let mut clicks: Vec<Value> = Vec::new();
        let p = params(&[("target", json!({"token": "e1"}))]);
        let e = dispatch(&mut state, "desk.click", &p, &mut |t, a| {
            if t == "click" {
                clicks.push(a.clone());
                return Err(Error::new(
                    "ACTION_REJECTED",
                    "AX action failed: AXUIElementPerformAction(AXPress) returned -25206",
                ));
            }
            script.call(t, a)
        })
        .unwrap_err();
        assert_eq!(e.code, "ACTION_REJECTED");
        assert!(e.message.contains("-25206"), "{}", e.message);
        assert_eq!(clicks.len(), 1);
    }

    /// The fallback trigger is one signature: the action never applied
    /// because the role cannot perform it (-25206, not any AX error).
    #[test]
    fn the_px_rung_triggers_only_on_unsupported_ax_press() {
        assert!(ax_press_unsupported(
            "AX action failed: AXUIElementPerformAction(AXPress) returned -25206"
        ));
        assert!(!ax_press_unsupported(
            "AX action failed: AXUIElementPerformAction(AXPress) returned -25202"
        ));
        assert!(!ax_press_unsupported("capture_stale"));
    }

    /// A token no look ever minted is a stale handle, not a guessable click.
    #[test]
    fn unknown_tokens_are_stale_not_fabricated() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new().answer("click", json!({"effect": "confirmed"}));
        let p = params(&[("target", json!({"token": "ghost"}))]);
        let e = dispatch(&mut state, "desk.click", &p, &mut |t, a| script.call(t, a)).unwrap_err();
        assert_eq!(e.code, "STALE_TARGET");
        assert!(e.message.contains("desk.win_state"), "{}", e.message);
    }

    /// win_state hands the model element handles plus the revision it bound.
    #[test]
    fn win_state_shapes_element_handles() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        let ws = dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(ws["window"], json!({"pid": 7, "window_id": 3}));
        assert_eq!(ws["revision"], json!("cap_w"));
        assert_eq!(ws["els"][0]["token"], json!("e1"));
        assert_eq!(ws["els"][0]["role"], json!("button"));
        assert_eq!(ws["els"][0]["text"], json!("Save"));
        assert_eq!(state.window_capture.as_ref().unwrap().1, "cap_w");
    }

    /// An empty AX tree is a degradation with a reason — pass the driver's
    /// verdict through instead of a silent "0 controls".
    #[test]
    fn win_state_surfaces_the_degradation_verdict() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut degraded = window_answer();
        degraded["elements"] = json!([]);
        degraded["degraded"] = json!(true);
        degraded["degraded_reason"] = json!("ax_window_unresolved: …");
        degraded["escalation"] = json!({"recommended": "foreground"});
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("get_window_state", degraded);
        let p = params(&[("window", Value::Null)]);
        let ws = dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(ws["els"], json!([]));
        assert_eq!(ws["degraded"], json!(true));
        assert_eq!(ws["degraded_reason"], json!("ax_window_unresolved: …"));
        assert_eq!(ws["escalation"]["recommended"], json!("foreground"));
    }

    /// The wire splits on delivery, not evidence strength: only `refused`
    /// rejects (nothing applied); a delivered act comes back readable with its
    /// verdict verbatim — `unverifiable` is delivered, merely not verified.
    #[test]
    fn effect_verdicts_split() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new().answer("click", json!({ "effect": "refused" }));
        let p = params(&[("target", json!([1, 2])), ("button", json!("left"))]);
        let e = dispatch(&mut state, "desk.click", &p, &mut |t, a| script.call(t, a)).unwrap_err();
        assert_eq!(e.code, "ACTION_REJECTED");
        for effect in ["confirmed", "partial", "unverifiable", "suspected_noop"] {
            let mut script = Script::new().answer("click", json!({ "effect": effect }));
            let p = params(&[("target", json!([1, 2])), ("button", json!("left"))]);
            let r = dispatch(&mut state, "desk.click", &p, &mut |t, a| script.call(t, a))
                .unwrap_or_else(|e| panic!("effect={effect} is delivered, not rejected: {e:?}"));
            assert_eq!(
                r["effect"],
                json!(effect),
                "the model reads the verdict verbatim"
            );
        }
    }

    /// No accessibility tree, no rewrite: the old z ranking still decides —
    /// active app first, on screen before not, maximum z in front.
    #[test]
    fn without_a_tree_the_z_ranking_still_decides() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer(
                "list_apps",
                json!({ "apps": [ {"pid": 7, "active": true} ] }),
            )
            .answer(
                "list_windows",
                json!({ "windows": [
                {"window_id": 3, "pid": 7, "is_on_screen": true, "z_index": 1},
                {"window_id": 4, "pid": 7, "is_on_screen": true, "z_index": 9},
                {"window_id": 5, "pid": 8, "is_on_screen": true, "z_index": 20},
                {"window_id": 6, "pid": 7, "is_on_screen": false, "z_index": 30},
            ] }),
            )
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        let ws = dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        // 6 is the active app's top z but off-screen; 4 is its top on-screen
        // window. The stranger at z 20 and the backmost at z 1 both lose.
        assert_eq!(ws["window"], json!({"pid": 7, "window_id": 4}));
    }

    /// The live fumble this ranking exists for: cua's z ranked a leftover
    /// TextEdit document (z 1085) above the fresh frontmost one (z 895) —
    /// both on screen, off-screen aux windows scrambling the all-windows
    /// list. The tree's array order is the truth: index 0 is the user's.
    #[test]
    fn focused_window_follows_the_tree_order_not_the_z_lie() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer(
                "list_windows",
                json!({ "windows": [
                {"window_id": 56037, "pid": 7, "is_on_screen": true, "z_index": 1085},
                {"window_id": 56039, "pid": 7, "is_on_screen": true, "z_index": 895},
                {"window_id": 55648, "pid": 7, "is_on_screen": false, "z_index": 1505},
            ] }),
            )
            .answer(
                "get_accessibility_tree",
                json!({ "windows": [
                {"window_id": 56039, "pid": 7},
                {"window_id": 56037, "pid": 7},
            ] }),
            )
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        let ws = dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(
            ws["window"],
            json!({"pid": 7, "window_id": 56039}),
            "the fresh frontmost document wins over the z lie and the aux junk"
        );
    }

    /// The tree only orders — it never re-ranks apps. The active app's
    /// invisible window still beats a stranger's visible one (macOS can
    /// report the editor's windows off-screen while it is the app in use).
    #[test]
    fn the_active_apps_off_tree_window_still_beats_a_stranger() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer(
                "list_apps",
                json!({ "apps": [ {"pid": 7, "active": true} ] }),
            )
            .answer(
                "list_windows",
                json!({ "windows": [
                {"window_id": 10, "pid": 7, "is_on_screen": false, "z_index": 1},
                {"window_id": 20, "pid": 8, "is_on_screen": true, "z_index": 20},
            ] }),
            )
            .answer(
                "get_accessibility_tree",
                json!({ "windows": [ {"window_id": 20, "pid": 8} ] }),
            )
            .answer("get_window_state", window_answer());
        let p = params(&[("window", Value::Null)]);
        let ws = dispatch(&mut state, "desk.win_state", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(ws["window"], json!({"pid": 7, "window_id": 10}));
    }

    /// The listing carries the same front rank the focused pick uses: higher
    /// z_index is closer to the user, off-tree windows sit at the floor.
    #[test]
    fn windows_re_stamp_z_as_the_true_front_rank() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer(
                "list_windows",
                json!({ "windows": [
                {"window_id": 56037, "pid": 7, "app_name": "ed", "title": "old", "z_index": 1085},
                {"window_id": 56039, "pid": 7, "app_name": "ed", "title": "new", "z_index": 895},
                {"window_id": 55648, "pid": 7, "app_name": "ed", "title": "aux", "z_index": 1505},
            ] }),
            )
            .answer(
                "get_accessibility_tree",
                json!({ "windows": [
                {"window_id": 56039, "pid": 7},
                {"window_id": 56037, "pid": 7},
            ] }),
            );
        let out = dispatch(&mut state, "desk.windows", &Map::new(), &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        let rows = out["windows"].as_array().unwrap();
        let z =
            |id: u64| rows.iter().find(|w| w["window_id"] == json!(id)).unwrap()["z_index"].clone();
        assert_eq!(z(56039), json!(2), "the frontmost document is the top rank");
        assert_eq!(z(56037), json!(1), "the leftover document ranks under it");
        assert_eq!(z(55648), json!(0), "off-tree windows sit at the floor");
    }

    /// Windowless input acts declare desktop scope — without it cua demands a
    /// pid and rejects every one of them.
    #[test]
    fn global_input_acts_declare_desktop_scope() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("get_screen_size", json!({"width": 1000.0, "height": 800.0}))
            .answer("click", json!({"effect": "unverifiable"}))
            .answer("move_cursor", json!({"effect": "unverifiable"}))
            .answer("drag", json!({"effect": "unverifiable"}))
            .answer("scroll", json!({"effect": "unverifiable"}))
            .answer("press_key", json!({"effect": "unverifiable"}))
            .answer("hotkey", json!({"effect": "unverifiable"}))
            .answer("type_text", json!({"effect": "unverifiable"}));
        for (op, p, tool) in [
            ("desk.click", params(&[("target", json!([1, 2]))]), "click"),
            (
                "desk.move",
                params(&[("target", json!([1, 2]))]),
                "move_cursor",
            ),
            (
                "desk.drag",
                params(&[("start", json!([1, 2])), ("end", json!([3, 4]))]),
                "drag",
            ),
            (
                "desk.scroll",
                params(&[("direction", json!("down")), ("target", Value::Null)]),
                "scroll",
            ),
            ("desk.key", params(&[("key", json!("a"))]), "press_key"),
            (
                "desk.hotkey",
                params(&[("keys", json!(["cmd", "s"]))]),
                "hotkey",
            ),
            ("desk.type", params(&[("text", json!("hi"))]), "type_text"),
        ] {
            dispatch(&mut state, op, &p, &mut |t, a| script.call(t, a))
                .unwrap_or_else(|e| panic!("{op} must dispatch: {e:?}"));
            assert_eq!(script.saw(tool)["scope"], json!("desktop"), "{op} → {tool}");
        }
    }

    /// No look, no parse: regions is honest about needing a capture.
    #[test]
    fn regions_follow_the_shots_picture() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("get_desktop_state", desktop_answer())
            .answer("parse_visual_regions", json!({"regions": []}));
        let p = Map::new();
        let e = dispatch(&mut state, "desk.regions", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap_err();
        assert_eq!(e.code, "STALE_TARGET");
        assert!(e.message.contains("look"));

        let p = params(&[
            ("mode", json!("desktop")),
            ("window", Value::Null),
            ("region", Value::Null),
        ]);
        dispatch(&mut state, "desk.shot", &p, &mut |t, a| script.call(t, a)).unwrap();
        let p = params(&[("shot", json!({"path": "/tmp/cap1.png"}))]);
        dispatch(&mut state, "desk.regions", &p, &mut |t, a| {
            script.call(t, a)
        })
        .unwrap();
        assert_eq!(
            script.saw("parse_visual_regions")["capture_id"],
            json!("cap_1")
        );
    }

    /// Menu paths are the seed's `"File > Save"`, cua's is a segment list.
    #[test]
    fn menu_paths_split_into_segments() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("list_apps", focused_apps())
            .answer("list_windows", focused_windows())
            .answer("invoke_menu", json!({"effect": "confirmed"}));
        let p = params(&[("path", json!("File > Save"))]);
        dispatch(&mut state, "desk.menu", &p, &mut |t, a| script.call(t, a)).unwrap();
        let args = script.saw("invoke_menu");
        assert_eq!(args["path"], json!(["File", "Save"]));
        assert_eq!(args["pid"], json!(7));
        assert_eq!(args["window_id"], json!(3));
    }

    /// The wire's amount is pixels-ish; cua counts lines 1–50.
    #[test]
    fn scroll_amount_becomes_lines() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("get_screen_size", json!({"width": 1000.0, "height": 800.0}))
            .answer("scroll", json!({"effect": "confirmed"}));
        let p = params(&[
            ("direction", json!("down")),
            ("amount", json!(400)),
            ("target", Value::Null),
        ]);
        dispatch(&mut state, "desk.scroll", &p, &mut |t, a| script.call(t, a)).unwrap();
        let args = script.saw("scroll");
        assert_eq!(args["x"], json!(500.0));
        assert_eq!(args["amount"], json!(10));
        assert_eq!(args["by"], json!("line"));
    }

    /// A targetless scroll aims at the screen centre **in the space the
    /// driver reads**: after a desktop look that is the published picture's
    /// pixels (the driver scales them back to native), not the screen's
    /// point size. Sending points here lands the scroll a quarter of the
    /// screen off.
    #[test]
    fn a_targetless_scroll_aims_at_the_published_picture_centre() {
        let mut state = State::new(PathBuf::from("/tmp"));
        let mut script = Script::new()
            .answer("get_desktop_state", desktop_answer())
            .answer("scroll", json!({"effect": "confirmed"}));
        let p = params(&[
            ("mode", json!("desktop")),
            ("window", Value::Null),
            ("region", Value::Null),
        ]);
        dispatch(&mut state, "desk.shot", &p, &mut |t, a| script.call(t, a)).unwrap();

        let p = params(&[
            ("direction", json!("down")),
            ("amount", json!(100)),
            ("target", Value::Null),
        ]);
        dispatch(&mut state, "desk.scroll", &p, &mut |t, a| script.call(t, a)).unwrap();
        let args = script.saw("scroll");
        // desktop_answer() publishes a 1440x900 picture.
        assert_eq!(args["x"], json!(720.0));
        assert_eq!(args["y"], json!(450.0));
        assert_eq!(args["scope"], json!("desktop"));
    }

    /// A deadline that may have landed is never a refusal; a read never
    /// pretends to have landed.
    #[test]
    fn deadlines_classify_by_mutability() {
        assert!(!mutates("desk.shot", &Map::new()));
        assert!(!mutates("desk.win_state", &Map::new()));
        assert!(!mutates("desk.clip_read", &Map::new()));
        assert!(mutates("desk.click", &Map::new()));
        assert!(mutates("desk.key", &Map::new()));
        assert!(!mutates(
            "desk.agent_cursor",
            &params(&[("op", json!("state"))])
        ));
        assert!(mutates(
            "desk.agent_cursor",
            &params(&[("op", json!("set"))])
        ));
        assert_eq!(
            deadline_error("desk.click", &Map::new()).code,
            "ACTION_OUTCOME_UNKNOWN"
        );
        assert_eq!(
            deadline_error("desk.shot", &Map::new()).code,
            "ACTION_REJECTED"
        );
    }

    /// Region mode cuts the model a piece; the file on disk is the crop.
    #[test]
    fn region_shot_writes_the_crop() {
        let shots = tempfile::tempdir().unwrap();
        let mut state = State::new(shots.path().to_path_buf());
        let mut script = Script::new();
        let p = params(&[
            ("mode", json!("region")),
            ("window", Value::Null),
            ("region", json!([5, 6, 7, 8])),
        ]);
        // Stand in for the driver: a real PNG at the out-file we pass in.
        let mut writer = |tool: &str, args: Value| -> Result<Value, Error> {
            if tool == "get_desktop_state" {
                let out = args["screenshot_out_file"].as_str().unwrap().to_string();
                let img = image::RgbImage::from_fn(40, 30, |_, _| image::Rgb([1, 2, 3]));
                img.save(&out)
                    .map_err(|e| Error::new("ACTION_REJECTED", e.to_string()))?;
                let mut r = desktop_answer();
                r["screenshot_file_path"] = json!(out);
                return Ok(r);
            }
            script.call(tool, args)
        };
        let handle = dispatch(&mut state, "desk.shot", &p, &mut writer).unwrap();
        assert_eq!(handle["width"], json!(7));
        assert_eq!(handle["height"], json!(8));
        let crop = handle["path"].as_str().unwrap();
        let img = image::open(crop).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (7, 8));
    }
}

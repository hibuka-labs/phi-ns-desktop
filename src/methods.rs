//! `desk.*` MethodSpec table — **the single declaration source** (§3.1).
//!
//! Every view of the pack (seed list, capability review, policy keys) is
//! derived from these rows; nothing else declares anything.
//!
//! Rows come from the O10 inventory (`agent-works/docs/p3-capability-inventory.md`):
//! the cua 29-tool manifest folded along the **policy-decision boundary** —
//! one `MethodSpec` name is one `MethodPolicy` row, so tools that ask and
//! tools that flow never share a name (the P2 lesson that let
//! `b.click` ask while `b.screenshot` flowed). Upstream dotted capabilities
//! stay in the inventory for provenance; the declaration vocabulary here is
//! only `phi_cap`.
//!
//! Shape decisions (inventory §4, self-ruled):
//!
//! * session start/end are **not** model-facing — the stub's lifetime is the
//!   config's lifetime (plug = configuration-level). Only `desk.session` reads
//!   and `desk.escalate` mutates, and escalate asks.
//! * the five `agent_cursor` tools merge into one `desk.agent_cursor` — same
//!   decision (Allow), same visual-hint nature.
//! * `desk.shot` absorbs the desktop/window/region capture modes; the window
//!   *tree* is `desk.win_state`, the window *picture* is `desk.shot(mode=…)`.
//!
//! Capture roles (inventory §3.2): the two lookers that yield a
//! capture-derived target (`desk.shot`, `desk.win_state`) mint one capture
//! per look, and the four pointer verbs that act on such a target spend one —
//! one action attempt per look, reuse refused at the chokepoint. Blind
//! keyboard input, menu paths, and window/clipboard effects draw no capture
//! authority (they bind to no element token or point from a look).

use phi_cap::{device_capture, device_input};
use phi_ns::{CaptureUse, MethodSpec, m};

/// The whole `desk.*` face, in seed-list order.
pub fn method_table() -> Vec<MethodSpec> {
    vec![
        // ---- look: captures flow through the gate, unasked (MethodPolicy) ----
        m(
            "desk.shot",
            &[device_capture(&["desktop"])],
            "shot(mode=\"desktop\", window=None, region=None): capture the screen, one window, or a region as an ImageHandle (path/mime/alt — a live object, not JSON: read its fields, never json.dumps it)",
            true,
        )
        .capture(CaptureUse::Mint),
        m(
            "desk.win_state",
            &[device_capture(&["desktop"])],
            "win_state(window=None): read one window's accessibility tree (roles, names, element tokens); window=None is the focused window; ids come from desk.windows() and expire per look",
            false,
        )
        .capture(CaptureUse::Mint),
        m(
            "desk.windows",
            &[device_capture(&["desktop"])],
            "windows(): list windows as [id] app title lines (ids are fresh per look — pass one straight to desk.win_state)",
            false,
        ),
        m(
            "desk.apps",
            &[device_capture(&["desktop"])],
            "apps(): list running applications",
            false,
        ),
        m(
            "desk.screen",
            &[device_capture(&["desktop"])],
            "screen(): read screen size and pointer position — returns a dict ('pointer' holds {'x','y'} in screen POINTS; desk.move/click want a LIST [x, y] in desktop-shot pixels, not a dict)",
            false,
        ),
        m(
            "desk.regions",
            &[device_capture(&["desktop"])],
            "regions(shot=None): parse a capture into named visual regions (buttons, fields, menus); shot=None uses the last capture; needs the optional cua-perception extension",
            false,
        ),
        m(
            "desk.verify",
            &[device_capture(&["desktop"])],
            "verify(claim): check a stated expectation against live screen/element state and report whether it holds",
            false,
        ),
        // ---- act: device input asks a human (MethodPolicy) ----
        m(
            "desk.click",
            &[device_input(&["desktop"])],
            "click(target, button=\"left\"): click an element token from desk.win_state or an [x, y] point (a LIST, not a dict)",
            false,
        )
        .capture(CaptureUse::Consume),
        m(
            "desk.drag",
            &[device_input(&["desktop"])],
            "drag(start, end): drag from start to end (element tokens or [x, y] points — lists, not dicts)",
            false,
        )
        .capture(CaptureUse::Consume),
        m(
            "desk.move",
            &[device_input(&["desktop"])],
            "move(target): move the REAL OS pointer to an element token from desk.win_state or an [x, y] LIST in DESKTOP-SHOT PIXELS (the PNG from desk.shot(mode=\"desktop\"), not screen points)",
            false,
        )
        .capture(CaptureUse::Consume),
        m(
            "desk.scroll",
            &[device_input(&["desktop"])],
            "scroll(direction, amount=400, target=None): scroll \"up\"/\"down\"/\"left\"/\"right\" by amount, optionally over a target",
            false,
        )
        .capture(CaptureUse::Consume),
        m(
            "desk.key",
            &[device_input(&["desktop"])],
            "key(key): press one key, e.g. \"Enter\" or \"esc\"",
            false,
        ),
        m(
            "desk.hotkey",
            &[device_input(&["desktop"])],
            "hotkey(keys): press a key combination, e.g. [\"cmd\", \"s\"]",
            false,
        ),
        m(
            "desk.type",
            &[device_input(&["desktop"])],
            "type(text): type text at the current focus",
            false,
        ),
        m(
            "desk.menu",
            &[device_input(&["desktop"])],
            "menu(path): invoke a native menu by path, e.g. \"File > Save\"",
            false,
        ),
        // ---- resources ----
        m(
            "desk.clip_read",
            &[device_capture(&["clipboard"])],
            "clip_read(): read the clipboard as text or an image summary",
            false,
        ),
        m(
            "desk.clip_write",
            &[device_input(&["clipboard"])],
            "clip_write(value): put text or an image path on the clipboard",
            false,
        ),
        // ---- session / window management ----
        m(
            "desk.win_frame",
            &[device_input(&["desktop"])],
            "win_frame(window, x, y, w, h): move and resize a window (window id from desk.windows())",
            false,
        ),
        m(
            "desk.session",
            &[device_capture(&["desktop"])],
            "session(op=\"get\"|\"list\"|\"state\"): read capture-session state (scopes, what is visible)",
            false,
        ),
        m(
            "desk.escalate",
            &[device_input(&["desktop"])],
            "escalate(): ask for a wider capture scope (more of the screen becomes visible); always reviewed",
            false,
        ),
        // ---- visual hint cursor (not device input) ----
        m(
            "desk.agent_cursor",
            &[device_input(&["desktop"])],
            "agent_cursor(op, ...): drive the visible agent cursor — op=\"state\" reads it, \"move\"/\"set\" place and style it so a human can follow the automation",
            false,
        ),
    ]
}

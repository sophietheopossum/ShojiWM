//! `~/.config/minka-settings.json`, owned by MinkaConf, and its `settings.*`
//! IPC.
//!
//! Read at runtime from the path MinkaConf and MinkaShell always use. Boot
//! reads it once; `settings.apply` over IPC swaps the whole object at runtime
//! and the input and output factories re-run, with no restart. A missing or
//! unparseable file falls back to defaults, so a fresh machine boots before
//! MinkaConf has ever written anything.
//!
//! The settings stay a JSON value: MinkaConf owns the whole file, and keys
//! this config never reads (`shell.*`, `minkaconf.*`) must come back from
//! `settings.get` untouched.

use std::{cell::RefCell, path::PathBuf};

use serde_json::{Value, json};
use shojiwm_rs::ipc::IpcServer;

/// Config-schema revision handshake. Bump whenever the settings schema or the
/// factories consuming it change, so MinkaConf can tell the running session
/// predates the edit instead of silently half-applying. History: 1 = input +
/// display scale; 2 = full display schema (position/mode/enabled/mirror/hdr);
/// 3 = cursor theme + size; 4 = display subpixel layout; 5 =
/// workspaces.enabled (virtual desktops on/off).
const MINKA_CONFIG_REVISION: u32 = 5;

thread_local! {
    static ACTIVE: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/minka-settings.json")
}

/// 8/7/2026 defaults per Sophie: adaptive accel, natural scroll off.
fn defaults() -> Value {
    json!({
        "input": {
            "pointerAccel": 0.4,
            "accelProfile": "adaptive",
            "naturalScroll": false,
            "touchpad": {
                "naturalScroll": false,
                "tapToClick": true,
                "scrollMethod": "twoFinger",
                "scrollFactor": 1,
                "disableWhileTyping": false,
            },
            "keyboard": {
                "layout": "us",
                "variant": "",
            },
        },
        "displays": {},
    })
}

fn load() -> Value {
    let path = settings_path();
    let parsed = std::fs::read_to_string(&path)
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()));
    match parsed {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(
                "minka-settings: using defaults (cannot read {}: {error})",
                path.display()
            );
            defaults()
        }
    }
}

/// Run `f` on the settings in effect. Factories must read them when they run
/// rather than keep them: `settings.apply` replaces the whole object.
pub fn with_current<R>(f: impl FnOnce(&Value) -> R) -> R {
    ACTIVE.with(|active| f(active.borrow_mut().get_or_insert_with(load)))
}

/// Only an explicit `false` turns virtual desktops off; MinkaShell reads the
/// same key to hide its desktop pills.
pub fn workspaces_enabled() -> bool {
    with_current(|settings| settings["workspaces"]["enabled"] != Value::Bool(false))
}

/// MinkaConf's side of the IPC socket. `on_apply` runs after every
/// `settings.apply` swap and re-runs whatever consumes the settings; its
/// argument says whether virtual desktops flipped on or off.
pub fn serve_settings(server: &IpcServer, on_apply: impl Fn(bool) + 'static) {
    server.handle("minka.revision", |_| {
        Ok(json!({ "revision": MINKA_CONFIG_REVISION }))
    });

    // Effective user settings, for MinkaConf to display.
    server.handle("settings.get", |_| Ok(with_current(Value::clone)));

    // Live-apply from MinkaConf: swap the active settings and re-run the
    // input and output factories. Takes effect immediately.
    server.handle("settings.apply", move |params| {
        // `typeof params === "object"` in the TypeScript config, which takes
        // arrays too and refuses null.
        if !(params.is_object() || params.is_array()) {
            return Ok(json!({ "ok": false, "error": "expected a settings object" }));
        }
        let desktops_were_enabled = workspaces_enabled();
        ACTIVE.with(|active| *active.borrow_mut() = Some(params.clone()));
        // MinkaConf sends the whole file on every edit: act only on a change.
        on_apply(workspaces_enabled() != desktops_were_enabled);
        Ok(json!({ "ok": true }))
    });
}

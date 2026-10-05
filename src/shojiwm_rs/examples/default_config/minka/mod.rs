//! Minka's config, one module per concern. Constructing them does nothing on
//! its own: `setup` in main.rs decides the order they run in, because the
//! order matters (the settings cursor must replace the default one, and the
//! window manager must exist before anything that drives it).
//!
//! - `settings`: minka-settings.json (MinkaConf), `settings.*` IPC
//! - `session`: environment, cursor, decoration mode, autostarted apps
//! - `workspace_ipc`: the IPC socket: `workspaces.*`, `windows.*`, broadcasts
//! - `keybinds`: key bindings, including the virtual-desktop keys
//! - `displays`: output layout; `input`: pointer, keyboard and gestures
//! - `rendering`: blur effects and surface policy
//! - `events`: compositor events to the window manager; `dock`: proximity
//! - `decoration`: window chrome and drag tabs

pub mod decoration;
pub mod displays;
pub mod dock;
pub mod events;
pub mod input;
pub mod keybinds;
pub mod rendering;
pub mod session;
pub mod settings;
pub mod workspace_ipc;

use serde_json::{Value, json};
use shojiwm_rs::prelude::Rect;

/// Numbers as JavaScript's `JSON.stringify` writes them: whole numbers
/// without a fraction (`100`, not `100.0`), so typed clients that read
/// integers parse what the TypeScript config used to send.
pub fn js_numbers(mut value: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Number(number) => {
                if let Some(float) = number.as_f64().filter(|_| number.is_f64())
                    && float.fract() == 0.0
                    && float.abs() < 9_007_199_254_740_992.0
                {
                    *value = Value::from(float as i64);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            Value::Object(fields) => fields.values_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut value);
    value
}

/// JavaScript truthiness, for settings the TypeScript config tested that way.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|number| number != 0.0 && !number.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub fn rect_json(rect: Rect) -> Value {
    json!({ "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height })
}

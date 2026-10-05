//! Pointer, touchpad and keyboard from minka-settings.json (MinkaConf), plus
//! the gesture speeds and the window move/resize modifiers.
//!
//! Like the display entries, the input config is built as the JSON the
//! TypeScript config handed the compositor, so settings values (the accel
//! profile and scroll method names among them) pass through unchanged and a
//! missing one stays unset.

use serde_json::{Value, json};
use shojiwm_rs::{
    prelude::*,
    runtime_input::{RuntimeInputConfig, RuntimeInputDeviceConfig, RuntimeKeyboardInputConfig},
};

use super::{js_numbers, settings, truthy};
use crate::window_manager::{WindowManager, WorkspaceGestureSpeed};

/// The global device config the settings ask for.
fn global_input(settings: &Value) -> Value {
    let input = &settings["input"];
    let touchpad = &input["touchpad"];
    let keyboard_settings = &input["keyboard"];
    let mut keyboard = json!({
        "layout": if truthy(&keyboard_settings["layout"]) { keyboard_settings["layout"].clone() } else { json!("us") },
        "options": "caps:ctrl_modifier",
        "repeatRate": 60,
        "repeatDelay": 250,
    });
    if truthy(&keyboard_settings["variant"]) {
        keyboard["variant"] = keyboard_settings["variant"].clone();
    }
    json!({
        "touchpad": {
            "tapToClick": touchpad["tapToClick"],
            "naturalScroll": touchpad["naturalScroll"],
            "scrollMethod": touchpad["scrollMethod"],
            "disableWhileTyping": touchpad["disableWhileTyping"],
            "scrollFactor": touchpad["scrollFactor"],
            "pointerAccel": input["pointerAccel"],
            "accelProfile": input["accelProfile"],
        },
        "pointer": {
            "pointerAccel": input["pointerAccel"],
            "accelProfile": input["accelProfile"],
            "naturalScroll": input["naturalScroll"],
        },
        "keyboard": keyboard,
    })
}

pub fn configure_input(wm: &WindowManager) {
    // 8/7/2026 defaults per Sophie: adaptive accel at +0.4 (the old flat 0.0
    // was "acceleration too low") and natural scroll off everywhere.
    COMPOSITOR
        .input
        .configure(|input: &mut RuntimeInputConfig, _devices| {
            let global = settings::with_current(global_input);
            input.global = match serde_json::from_value(js_numbers(global)) {
                Ok(global) => Some(global),
                Err(error) => {
                    tracing::warn!("minka-settings: ignoring the input settings: {error}");
                    None
                }
            };
            input.device.insert(
                "Razer Razer Blade Keyboard".into(),
                Some(RuntimeInputDeviceConfig {
                    keyboard: Some(RuntimeKeyboardInputConfig {
                        layout: Some("us".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            );
        });

    wm.with(|wm| {
        wm.configure_workspace_gesture_speed(WorkspaceGestureSpeed {
            workspace_scroll_factor: 1.5,
            workspace_scroll_kinetic_factor: 1.0,
            workspace_switch_factor: 1.0,
            workspace_switch_velocity_factor: 1.0,
            // At or below this scroll speed (logical px/s) the workspace
            // scroll catches on tile snap positions (fully-on-screen edges;
            // the centre for maximized tiles). 0 disables snapping.
            workspace_scroll_snap_max_velocity: 600.0,
            // Finger travel (logical px) needed to break out of a catch.
            workspace_scroll_snap_breakout_px: 48.0,
        })
    });

    COMPOSITOR.pointer.bind_window_move_modifier("Super");
    COMPOSITOR.pointer.bind_window_resize_modifier("Super");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_become_the_global_input_config() {
        let settings = json!({ "input": {
            "pointerAccel": 0.4, "accelProfile": "adaptive", "naturalScroll": false,
            "touchpad": {
                "naturalScroll": true, "tapToClick": true, "scrollMethod": "twoFinger",
                "scrollFactor": 1, "disableWhileTyping": false
            },
            "keyboard": { "layout": "gb", "variant": "" }
        }});
        let global: RuntimeInputDeviceConfig =
            serde_json::from_value(js_numbers(global_input(&settings)))
                .expect("valid input settings");
        let keyboard = global.keyboard.expect("keyboard");
        assert_eq!(keyboard.layout.as_deref(), Some("gb"));
        assert_eq!(keyboard.variant, None);
        assert_eq!(keyboard.options.as_deref(), Some("caps:ctrl_modifier"));
        assert_eq!(
            (keyboard.repeat_rate, keyboard.repeat_delay),
            (Some(60), Some(250))
        );
        let touchpad = global.touchpad.expect("touchpad");
        assert_eq!(touchpad.natural_scroll, Some(true));
        assert_eq!(touchpad.pointer_accel, Some(0.4));

        // A settings file from before the keyboard section still loads.
        let old: RuntimeInputDeviceConfig =
            serde_json::from_value(js_numbers(global_input(&json!({ "input": {} }))))
                .expect("defaults");
        assert_eq!(
            old.keyboard.expect("keyboard").layout.as_deref(),
            Some("us")
        );
    }
}

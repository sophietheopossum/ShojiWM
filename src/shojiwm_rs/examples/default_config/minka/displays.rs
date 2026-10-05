//! Output layout, fully user-managed through MinkaConf (minka-settings.json):
//! scale, explicit position, mode, enable/disable, mirroring, the HDR opt-in
//! (HDR only engages when the sink's EDID advertises PQ) and the subpixel
//! layout. Connectors with no entry get KDE-parity defaults: best mode, auto
//! position, scale 1.0.
//!
//! Each output's entry is built as JSON, the shape the TypeScript config
//! handed the compositor, then parsed into `RuntimeOutputConfig`. So an unset
//! key stays absent (the compositor then falls back to EDID and its own
//! constants rather than to 0), and a key the compositor build does not know
//! (the HDR ones, on a build without the HDR pipeline) is ignored instead of
//! failing to compile.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use shojiwm_rs::{config::RuntimeOutputConfig, prelude::*};

use super::{js_numbers, settings, truthy};

const OUTPUT_SUBPIXELS: [&str; 6] = [
    "unknown",
    "none",
    "horizontal-rgb",
    "horizontal-bgr",
    "vertical-rgb",
    "vertical-bgr",
];

/// The compositor parses the layout strictly, and one unrecognised value
/// would fail the whole entry rather than just this key. MinkaConf only
/// writes valid names, so this guards hand edits: drop the value and say so.
fn subpixel_setting(name: &str, value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(layout) if OUTPUT_SUBPIXELS.contains(&layout.as_str()) => Some(value.clone()),
        other => {
            tracing::warn!("minka-settings: ignoring unknown subpixel layout for {name}: {other}");
            None
        }
    }
}

fn display_entry(name: &str, entry: &Value) -> Value {
    if entry["enabled"] == Value::Bool(false) {
        return json!({ "mode": "disabled" });
    }
    if truthy(&entry["mirror"]) {
        let mut draft = json!({ "mode": "mirror", "source": entry["mirror"] });
        if let Some(subpixel) = subpixel_setting(name, &entry["subpixel"]) {
            draft["subpixel"] = subpixel;
        }
        return draft;
    }
    let resolution = match &entry["resolution"] {
        Value::Null => json!("best"),
        resolution => resolution.clone(),
    };
    let position = match &entry["position"] {
        Value::Object(position) => json!({ "x": position.get("x"), "y": position.get("y") }),
        _ => json!("auto"),
    };
    let scale = match &entry["scale"] {
        Value::Null => json!(1.0),
        scale => scale.clone(),
    };
    let mut draft = json!({
        "mode": "extend",
        "resolution": resolution,
        "position": position,
        "scale": scale,
        "hdr": entry["hdr"] == Value::Bool(true),
    });
    // Left out when unset: the compositor falls back to EDID, then to its
    // own constants, so an absent key must stay absent rather than become 0.
    for key in ["hdrMaxLuminance", "hdrMinLuminance"] {
        if !entry[key].is_null() {
            draft[key] = entry[key].clone();
        }
    }
    // Left out when unset too: the compositor keeps the layout the kernel
    // reported.
    if let Some(subpixel) = subpixel_setting(name, &entry["subpixel"]) {
        draft["subpixel"] = subpixel;
    }
    draft
}

/// The display configuration for `context`, as JSON drafts.
fn display_drafts(settings: &Value, context: &OutputContext) -> BTreeMap<String, Value> {
    let displays = &settings["displays"];
    let mut names: Vec<String> = displays
        .as_object()
        .map(|displays| displays.keys().cloned().collect())
        .unwrap_or_default();
    for output in &context.connected {
        if !names.contains(&output.name) {
            names.push(output.name.clone());
        }
    }

    let mut drafts: BTreeMap<String, Value> = names
        .into_iter()
        .map(|name| {
            let draft = display_entry(&name, &displays[name.as_str()]);
            (name, draft)
        })
        .collect();

    // Lid-closed docked mode: the external monitor replaces the built-ins.
    if context
        .connected
        .iter()
        .any(|output| output.name == "HDMI-A-1")
    {
        drafts.insert("eDP-1".to_owned(), json!({ "mode": "disabled" }));
        drafts.insert("eDP-2".to_owned(), json!({ "mode": "disabled" }));
    }

    // Keep the live layout's bounding box anchored at (0,0), like xrandr
    // does. X11 toolkits reading RandR through the Xwayland bridge assume the
    // screen starts at the top-left monitor corner (GTK clips menu workareas
    // against it). MinkaConf normalizes the arrangement it saves, but only
    // across the displays connected at the time, so a disconnect (the TV
    // owning the top-left corner, say) can leave the rest with a floating
    // origin. Translating every output by the same delta keeps the
    // arrangement; "auto" positions are left to the compositor.
    //
    // This works on the drafts, which own copies of the positions: the
    // settings themselves are never touched. Mutating them (10/9/2026, the
    // TypeScript config) rewrote the saved layout in memory on every unplug,
    // and the TV came back on top of both panels.
    let connected: Vec<&str> = context
        .connected
        .iter()
        .map(|output| output.name.as_str())
        .collect();
    let positioned: Vec<String> = drafts
        .iter()
        .filter(|(name, draft)| {
            draft["mode"] == "extend"
                && connected.contains(&name.as_str())
                && draft["position"].is_object()
        })
        .map(|(name, _)| name.clone())
        .collect();
    let coordinate = |name: &String, axis: &str| drafts[name]["position"][axis].as_f64();
    let min = |axis: &str| {
        positioned
            .iter()
            .filter_map(|name| coordinate(name, axis))
            .fold(None, |min: Option<f64>, value| {
                Some(min.map_or(value, |min| min.min(value)))
            })
    };
    if let (Some(min_x), Some(min_y)) = (min("x"), min("y"))
        && (min_x != 0.0 || min_y != 0.0)
    {
        for name in &positioned {
            let position = &mut drafts.get_mut(name).expect("listed above")["position"];
            for (axis, delta) in [("x", min_x), ("y", min_y)] {
                if let Some(value) = position[axis].as_f64() {
                    position[axis] = json!(value - delta);
                }
            }
        }
    }
    drafts
}

pub fn configure_displays() {
    COMPOSITOR.output.configure(|context| {
        let drafts = settings::with_current(|settings| display_drafts(settings, context));
        drafts
            .into_iter()
            .filter_map(|(name, draft)| {
                match serde_json::from_value::<RuntimeOutputConfig>(js_numbers(draft)) {
                    Ok(config) => Some((name, Some(config))),
                    Err(error) => {
                        tracing::warn!(
                            "minka-settings: ignoring the display entry for {name}: {error}"
                        );
                        None
                    }
                }
            })
            .collect()
    });
}

#[cfg(test)]
mod tests {
    use shojiwm_rs::ssd::WaylandOutputSnapshot;

    use super::*;

    fn context(names: &[&str]) -> OutputContext {
        let connected: Vec<WaylandOutputSnapshot> = names
            .iter()
            .map(|name| crate::tests::output(name, 0))
            .collect();
        OutputContext {
            current: connected
                .iter()
                .map(|output| (output.name.clone(), output.clone()))
                .collect(),
            connected,
        }
    }

    #[test]
    fn positions_are_anchored_on_copies() {
        let settings = json!({ "displays": {
            "eDP-1": { "position": { "x": 192, "y": 1080 }, "scale": 1.25 },
            "DP-1": { "position": { "x": 0, "y": 1944 } },
            "HDMI-A-3": { "position": { "x": 0, "y": 0 }, "hdr": true, "hdrMaxLuminance": 600 },
        }});
        // The TV unplugged: only the two panels re-anchor to (0,0).
        let drafts = display_drafts(&settings, &context(&["eDP-1", "DP-1"]));
        assert_eq!(drafts["eDP-1"]["position"], json!({ "x": 192.0, "y": 0.0 }));
        assert_eq!(drafts["DP-1"]["position"], json!({ "x": 0.0, "y": 864.0 }));
        // The TV's own entry, and the settings, are untouched.
        assert_eq!(drafts["HDMI-A-3"]["position"], json!({ "x": 0, "y": 0 }));
        assert_eq!(
            settings["displays"]["eDP-1"]["position"],
            json!({ "x": 192, "y": 1080 })
        );
        assert_eq!(drafts["HDMI-A-3"]["hdrMaxLuminance"], json!(600));
        assert!(drafts["eDP-1"].get("hdrMaxLuminance").is_none());

        let config: RuntimeOutputConfig =
            serde_json::from_value(js_numbers(drafts["eDP-1"].clone())).expect("a valid entry");
        assert_eq!(config.scale, Some(1.25));
    }

    #[test]
    fn docking_disables_the_panels_and_bad_subpixels_are_dropped() {
        let settings = json!({ "displays": {
            "eDP-1": { "subpixel": "diagonal" },
            "DP-1": { "mirror": "eDP-1", "subpixel": "horizontal-bgr" },
            "DP-2": { "enabled": false },
        }});
        let drafts = display_drafts(&settings, &context(&["eDP-1", "HDMI-A-1"]));
        assert_eq!(drafts["eDP-1"], json!({ "mode": "disabled" }));
        assert_eq!(drafts["eDP-2"], json!({ "mode": "disabled" }));
        assert_eq!(drafts["DP-2"], json!({ "mode": "disabled" }));
        assert_eq!(
            drafts["DP-1"],
            json!({ "mode": "mirror", "source": "eDP-1", "subpixel": "horizontal-bgr" })
        );
        assert_eq!(drafts["HDMI-A-1"]["resolution"], json!("best"));
        assert_eq!(drafts["HDMI-A-1"]["position"], json!("auto"));
        assert!(drafts["HDMI-A-1"].get("subpixel").is_none());
    }
}

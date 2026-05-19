use std::process::{Command, Output};

use tracing::{debug, warn};

const ACTIVATION_ENV_KEYS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
];

pub fn publish_activation_environment(reason: &'static str) {
    if std::env::var_os("SHOJI_PUBLISH_ACTIVATION_ENV")
        .is_some_and(|value| value == "0" || value == "off")
    {
        return;
    }

    let keys = ACTIVATION_ENV_KEYS
        .iter()
        .copied()
        .filter(|key| std::env::var_os(key).is_some())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return;
    }

    let systemd_result = run_dbus_update_activation_environment(&["--systemd"], &keys);
    match systemd_result {
        Ok(output) if output.status.success() => {
            debug!(?keys, reason, "published systemd activation environment");
        }
        Ok(output) => {
            debug!(
                ?keys,
                reason,
                status = ?output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "systemd activation environment update failed; trying D-Bus fallback"
            );
        }
        Err(error) => {
            debug!(
                ?keys,
                reason,
                ?error,
                "failed to run systemd activation environment update; trying D-Bus fallback"
            );
        }
    }

    match run_dbus_update_activation_environment(&[], &keys) {
        Ok(output) if output.status.success() => {
            debug!(?keys, reason, "published D-Bus activation environment");
        }
        Ok(output) => {
            warn!(
                ?keys,
                reason,
                status = ?output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "failed to publish D-Bus activation environment"
            );
        }
        Err(error) => {
            warn!(
                ?keys,
                reason,
                ?error,
                "failed to run dbus-update-activation-environment"
            );
        }
    }
}

fn run_dbus_update_activation_environment(
    flags: &[&str],
    keys: &[&str],
) -> std::io::Result<Output> {
    Command::new("dbus-update-activation-environment")
        .args(flags)
        .args(keys)
        .output()
}

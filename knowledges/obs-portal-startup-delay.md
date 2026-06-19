# OBS Startup Delay from xdg-desktop-portal-gtk

## Summary

OBS could take tens of seconds to start after launching a fresh ShojiWM session.
The delay was not caused by OBS's Wayland surface creation or by ShojiWM's
ScreenCast portal implementation. The slow path was
`xdg-desktop-portal-gtk`: it took about 50 seconds to claim its D-Bus name when
started under ShojiWM, and OBS blocked while `xdg-desktop-portal` initialized
portal backends.

The working fix is to make sure the GTK portal service receives:

```sh
GDK_DEBUG_PORTALS=1
```

This replaces the older `GTK_USE_PORTAL=1` knob for current GTK versions.

## Observed Behavior

- OBS started slowly only on the first launch after compositor startup.
- Running `dist/install-portal.sh --no-build` after compositor startup also
  blocked for tens of seconds while restarting `xdg-desktop-portal`.
- After that restart completed, OBS launched quickly.
- Hyprland did not reproduce the issue.

The important clue was that the install script blocked at:

```sh
systemctl --user restart xdg-desktop-portal
```

That meant OBS was not uniquely slow; it was just the first application to pay
the portal cold-start cost.

## Log Findings

Before the fix, journal showed `xdg-desktop-portal` selecting the GTK backend
and then timing out while D-Bus activated it:

```text
Choosing gtk.portal for org.freedesktop.impl.portal.Settings as a last-resort fallback
Failed to create settings proxy: ... StartServiceByName ... timed out
```

After moving Settings/Lockdown away from GTK, the same pattern appeared for
other GTK-backed interfaces:

```text
Choosing gtk.portal for org.freedesktop.impl.portal.FileChooser as a last-resort fallback
Failed to create file chooser proxy: ... timed out
Choosing gtk.portal for org.freedesktop.impl.portal.AppChooser as a last-resort fallback
Failed to create app chooser proxy: ... timed out
```

This confirmed that the core issue was not a specific portal interface. The GTK
portal process itself was slow to finish startup and claim:

```text
org.freedesktop.impl.portal.desktop.gtk
```

With `GDK_DEBUG_PORTALS=1` in the user systemd environment, the GTK portal
started immediately and the timeout disappeared. The normal `default=shojiwm`
configuration could be restored.

## Current Working Configuration

The installed ShojiWM portal advertises only ScreenCast:

```ini
[portal]
DBusName=org.freedesktop.impl.portal.desktop.shojiwm
Interfaces=org.freedesktop.impl.portal.ScreenCast
UseIn=ShojiWM
```

The per-user portal preference is:

```ini
[preferred]
default=shojiwm
org.freedesktop.impl.portal.ScreenCast=shojiwm
```

With this setup, interfaces not implemented by ShojiWM can still fall back to
GTK when needed, including FileChooser.

## Environment Requirement

`GDK_DEBUG_PORTALS=1` must be visible to user systemd services, not only to the
interactive shell that starts OBS. Verify it with:

```sh
systemctl --user show-environment | grep GDK_DEBUG_PORTALS
```

The expected output is:

```text
GDK_DEBUG_PORTALS=1
```

If it is missing, import it into the user manager before restarting portals:

```sh
export GDK_DEBUG_PORTALS=1
systemctl --user import-environment GDK_DEBUG_PORTALS
systemctl --user restart xdg-desktop-portal
```

For a persistent setup, place it in an environment file such as:

```text
~/.config/environment.d/shojiwm-portals.conf
```

with:

```ini
GDK_DEBUG_PORTALS=1
```

## Rejected Workarounds

### `default=none`

Setting `default=none` and explicitly listing only ShojiWM/wlr-backed portals
avoided GTK startup, but it also disabled generic portals such as FileChooser.
That can break applications that expect portal-based file dialogs. This was a
diagnostic workaround only and should not be used as the final fix.

### Implementing Minimal Settings/Lockdown in ShojiWM

Adding lightweight Settings/Lockdown implementations to ShojiWM avoided the
first GTK timeout, but `xdg-desktop-portal` then fell through to GTK for
FileChooser/AppChooser and hit the same delay there. This showed that the
underlying problem was GTK portal startup, not missing ShojiWM interfaces.

## Practical Check

After restarting portals, a healthy startup log should show GTK and ShojiWM
portal services starting in the same second, without `StartServiceByName`
timeouts:

```text
Starting Portal service (GTK/GNOME implementation)...
Started Portal service (GTK/GNOME implementation).
Starting Portal service (ShojiWM implementation)...
Started Portal service (ShojiWM implementation).
Started Portal service.
```

If OBS startup becomes slow again, first check whether `GDK_DEBUG_PORTALS=1` is
still present in `systemctl --user show-environment`.

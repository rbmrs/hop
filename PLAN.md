# Hop — plan

Hop is a small cross-platform tray app that switches a monitor's active input
(USB-C, DP, HDMI, …) over DDC/CI with global hotkeys. It replaces a manual
macOS Shortcuts + `m1ddc` setup.

## Context

- Monitor: **DELL U3223QE** (single monitor today). It has a built-in KVM:
  a USB keyboard dongle plugged into the monitor follows the active input.
- Devices today:
  - **USB-C** → MacBook
  - **HDMI** → Linux box
  - DP → previously used, now free
- Old setup to replace: two macOS Shortcuts, "Monitor: USB-C" and
  "Monitor: DP", bound to ⌃⌥⌘= and ⌃⌥⌘-. They probably call
  `/opt/homebrew/bin/m1ddc set input <code>`. The "DP" one is stale: it must
  now target HDMI. Delete both once Hop works so hotkeys don't fire twice.
- Problem Hop solves on the Linux side: when the KVM routes the keyboard to
  Linux, the Mac hotkey can't fire. The user falls back to Bluetooth on the
  Logitech keyboard to reach the Mac. Running Hop on Linux too removes this.

## Decisions

| Topic | Decision |
|---|---|
| Name | Hop |
| Stack | Cross-platform from day one: **Tauri + Rust core** |
| Platforms | macOS first (ship and polish), then Linux |
| Form | Tray / menu bar app, icon only, no Dock icon, launch at login |
| Monitors | Single monitor, but data model binds hotkeys to (monitor, port) so more monitors work later |
| Hotkeys | One global hotkey per port, re-recordable in the UI. No cycle key |
| Defaults | ⌃⌥⌘= → USB-C ("MacBook"), ⌃⌥⌘- → HDMI ("Linux") |
| Feedback | None on switch; the screen changing is the feedback |
| Port discovery | Read the DDC capabilities string, parse VCP 0x60 (input source) values |
| Port labels | Detected names (USB-C, DP 1, HDMI 1…), user can rename |
| Unused ports | Show all detected, user can hide individual ports |
| Fallback | "Advanced → Add port manually": pick a standard input or enter a raw VCP 0x60 code |
| Current input | Tray menu shows a checkmark on the active port (read VCP 0x60) |
| CLI | `hop switch <port>` and `hop list` subcommands (needed for Linux/Wayland keybindings) |
| Repo | `~/dev/hop`, public GitHub repo `rbmrs/hop`, work on `main` |
| Install (mac) | Hop.app in `/Applications`, ad-hoc signed, personal use |

## Architecture

- **Rust core crate** (`hop-core`):
  - DDC backend trait: `list_displays`, `capabilities`, `get_input`, `set_input`.
  - macOS backend: IOKit / IOAVService DDC (Apple Silicon), same approach as
    `m1ddc`. No dependency on Homebrew `m1ddc`.
  - Linux backend: `/dev/i2c-*` DDC (e.g. the `ddc-i2c` / `ddc-hi` crates).
    Needs the `i2c-dev` module and user in the `i2c` group; document this.
  - Capabilities parser for VCP 0x60 values → named ports.
  - Config model + load/save.
- **Tauri app**: tray icon + menu (ports list, checkmark, Settings, Quit) and
  a settings window (ports: rename / hide / hotkey recorder / add manually).
- **CLI**: same binary or a sibling binary exposing `switch` and `list`.

## Config

A single JSON/TOML file in the platform config dir, same schema on both OSes
so it can be copied between machines. Sketch:

```toml
[[monitor]]
id = "DELL U3223QE"          # match by model/serial, not volatile UUID

  [[monitor.port]]
  code = 27                  # VCP 0x60 value
  label = "MacBook"
  hidden = false
  hotkey = "Ctrl+Alt+Cmd+Equal"

  [[monitor.port]]
  code = 17
  label = "Linux"
  hidden = false
  hotkey = "Ctrl+Alt+Cmd+Minus"
```

Verify the real codes on the U3223QE during discovery. Common Dell values:
DP1 = 15, HDMI1 = 17, USB-C = 27. Note: `m1ddc get input` returned `110`
during investigation, so don't trust readback blindly; verify.

## Known risks

- Capabilities strings can be incomplete or wrong → manual-add fallback.
- Wayland restricts global hotkeys. On GNOME/KDE use the GlobalShortcuts
  portal where available; otherwise the user binds a system shortcut to
  `hop switch <port>`.
- macOS global hotkeys may need Accessibility/Input Monitoring permission
  depending on the API used. Prefer an API that does not need it.
- Switching away from the Mac's input must still leave DDC reachable to
  switch back (works today with m1ddc over USB-C).

## Milestones

1. Rust core + CLI on macOS: list displays, read capabilities, get/set input.
   Verify codes on the Dell.
2. Tauri tray app on macOS: menu, settings window, hotkey recorder, config,
   launch at login. Install to /Applications. Replace old Shortcuts.
3. Create public GitHub repo `rbmrs/hop`, push.
4. Linux backend + tray on the Linux box, Wayland hotkey path, docs for
   i2c permissions.

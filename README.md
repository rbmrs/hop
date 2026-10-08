# Hop

Hop is a menu bar app that switches a monitor's input (USB-C, DP, HDMI, …)
over DDC/CI, from a tray menu or a global hotkey. It also has a `hop` CLI.
See `PLAN.md` for the design.

## Build

Requirements: Rust 1.99 or newer and the Tauri CLI. Run every command from
the repo root.

```sh
cargo install tauri-cli --version "^2" --locked
```

Build the CLI and run the tests:

```sh
cargo build --release -p hop
cargo test
```

Build the app bundle (ad-hoc signed, for personal use):

```sh
(cd crates/hop-app && cargo tauri build)
```

The bundle is `target/release/bundle/macos/Hop.app`.

## Install on macOS

1. Quit Hop if it runs.
2. Copy the bundle to `/Applications`:

   ```sh
   rm -rf /Applications/Hop.app
   cp -R target/release/bundle/macos/Hop.app /Applications/
   ```

3. Open `/Applications/Hop.app`. On its first run from `/Applications`, Hop
   turns on **Open at Login**. You can turn it off in the tray menu.
4. Optional: put the CLI on your `PATH`:

   ```sh
   cp target/release/hop /usr/local/bin/hop
   ```

Hop needs no Accessibility or Input Monitoring permission.

## Replace the old Shortcuts

Hop's default hotkeys are ⌃⌥⌘= (USB-C) and ⌃⌥⌘- (HDMI). In the Shortcuts
app, delete the old "Monitor: USB-C" and "Monitor: DP" shortcuts, and any
other app hotkey on the same keys. macOS lets two apps hold the same hotkey,
so both would fire and Hop cannot detect it.

## Config

The config is `~/Library/Application Support/hop/config.toml` on macOS and
`$XDG_CONFIG_HOME/hop/config.toml` on Linux. `$HOP_CONFIG` overrides the
path. Edit labels, hidden ports and hotkeys in **Settings…** in the tray menu.

## CLI

```sh
hop list            # monitors, ports, and the active input (*)
hop switch Linux    # by label, by detected name ("HDMI"), or by code ("17", "0x11")
```

## Troubleshooting

If a release build fails with `mis-aligned LINKEDIT string pool` or
`can't find crate for …_macros`, the Apple linker wrote a bad proc-macro
library. It happened with ld-27037 and Rust 1.97. Update Rust, delete
`target/release`, and build again.

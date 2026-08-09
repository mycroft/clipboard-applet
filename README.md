# Clipboard Applet

A small Rust tray applet for viewing and manipulating Wayland primary and regular clipboards. It provides configurable mouse actions, clipboard previews, a right-click action menu, and optional desktop notifications.

## Features

- Polls and previews both text clipboards in the tray tooltip.
- Copies, switches, and clears either or both clipboards.
- Configurable left- and middle-click actions.
- Right-click menu with state-aware clipboard and notification actions.
- Shared in-memory LIFO stack with push and pop-to-one-or-both operations.
- Optional content hiding, notifications, and debug logging.

## Requirements

- Rust 1.85 or newer (edition 2024)
- A Wayland compositor supporting `ext-data-control` or `wlr-data-control`; primary selection requires the appropriate primary-selection support
- A D-Bus session and StatusNotifier host, such as KDE Plasma or GNOME with an AppIndicator extension

## Build and run

```sh
cargo build --release
./target/release/clipboard-applet
```

Useful options:

```text
-c, --config-file <PATH>  Use a specific configuration file
-d, --debug               Log actions to stderr without logging clipboard text
    --with-notifications  Enable desktop notifications for this run
-h, --help                Show usage
```

Run checks with:

```sh
cargo test --locked
cargo clippy --locked -- -D warnings
```

## Configuration

The app reads `$XDG_CONFIG_HOME/clipboard-applet/config.toml`, falling back to `~/.config/clipboard-applet/config.toml`. Without a file, built-in defaults are used.

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/clipboard-applet"
cp contrib/config.toml "${XDG_CONFIG_HOME:-$HOME/.config}/clipboard-applet/config.toml"
```

Default configuration:

```toml
polling_period_ms = 1000
hide_content = false
notifications = false
icon_name = "edit-paste"
stack_size = 16
left_click = "COPY_PRIMARY"
middle_click = "SWITCH"
```

Mouse actions are:

| Action | Effect |
|---|---|
| `COPY_PRIMARY` | Copy primary to regular |
| `COPY_REGULAR` | Copy regular to primary |
| `SWITCH` | Swap primary and regular |
| `RESET` | Clear both clipboards |

When `hide_content` is enabled, previews show only character counts. Notifications are sent after successful actions when `notifications` is enabled.

`icon_name` is a Freedesktop icon-theme name resolved by the desktop, not a Unicode character or image path.

`stack_size` limits the shared in-memory clipboard stack to between 1 and 16 text entries. The stack is cleared when the applet exits.

## Tray controls

- Left and middle click perform their configured actions.
- Right click opens the action menu.
- Copy, Stack, Clear, Switch, and Reset entries are disabled when their required clipboard content is empty.
- Stack entries are listed newest-first unless `hide_content` is enabled. Pop can target primary, regular, or both; an entry is removed only after the write succeeds.
- When the stack is empty, pop actions are replaced by `No stacked entries yet`.
- `Enable notifications` / `Disable notifications` changes notification behavior for the current process without modifying the configuration file.
- `Exit` shuts down the applet. Wayland clipboard values owned by this process may disappear unless a clipboard manager retains them.

See [contrib/config.toml](contrib/config.toml) for the complete documented configuration.

## References

- [ksni](https://docs.rs/ksni/) — StatusNotifierItem tray integration
- [wl-clipboard-rs](https://docs.rs/wl-clipboard-rs/) — Wayland clipboard access

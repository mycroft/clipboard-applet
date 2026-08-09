# Clipboard Applet

A small Rust tray applet for viewing and manipulating Wayland primary and regular clipboards. It provides configurable mouse actions, clipboard previews, a right-click action menu, and optional desktop notifications.

## Features

- Monitors clipboard changes using Wayland events, with configurable polling fallback.
- Copies, switches, and clears either or both clipboards.
- Configurable left- and middle-click actions.
- Right-click menu with state-aware clipboard and notification actions.
- Shared in-memory LIFO stack with push and pop-to-one-or-both operations.
- Optional editing of clipboard selections and stacked text entries using an external editor.
- Optional content hiding, notifications, and debug logging.
- Single-instance protection for each desktop session.

## Requirements

- Rust 1.85 or newer (edition 2024)
- A Wayland compositor supporting `ext-data-control` or `wlr-data-control`; primary selection requires the appropriate primary-selection support
- A D-Bus session and StatusNotifier host, such as KDE Plasma or GNOME with an AppIndicator extension
- `XDG_RUNTIME_DIR` set by the desktop session

## Build and run

```sh
cargo build --release
./target/release/clipboard-applet
```

Useful options:

```text
-c, --config-file <PATH>  Use a specific configuration file
-d, --debug               Log actions to stderr without logging clipboard text
    --with-notifications <MODE>  Notification mode: true, false, or all
-h, --help                Show usage
```

Run checks with:

```sh
cargo test --locked
cargo clippy --locked -- -D warnings
```

## Installation and desktop startup

Install the release binary for the current user:

```sh
make install
```

Choose one startup method. For desktop autostart:

```sh
make install-autostart
```

The applet starts on the next desktop login. This method requires `~/.local/bin` in the desktop session's `PATH`.

Alternatively, install and enable the systemd user service:

```sh
make enable-systemd
```

Inspect it with `systemctl --user status clipboard-applet.service`. The service is tied to `graphical-session.target`. Do not enable both startup methods; single-instance protection prevents corruption, but the second launcher will fail.

Remove the binary and both integration methods with:

```sh
make uninstall
```

SIGINT, SIGTERM, and the tray Exit action all leave the main event loop cleanly. As with any exit, Wayland clipboard values owned by the applet may disappear unless a clipboard manager retains them.

## Configuration

The app reads `$XDG_CONFIG_HOME/clipboard-applet/config.toml`, falling back to `~/.config/clipboard-applet/config.toml`. Without a file, built-in defaults are used.

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/clipboard-applet"
cp contrib/config.toml "${XDG_CONFIG_HOME:-$HOME/.config}/clipboard-applet/config.toml"
```

Default configuration:

```toml
update_method = "EVENTS"
polling_period_ms = 1000
hide_content = false
notifications = false
notify_on_change = false
icon_name = "edit-paste"
editor = []
stack_enabled = true
stack_size = 16
max_clipboard_bytes = 1048576
max_stack_entry_bytes = 1048576
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

Set `notify_on_change = true` to notify when the primary or regular clipboard value changes. Change notifications are disabled by default and also require notifications to be enabled. The initial clipboard snapshot does not produce a notification.

`--with-notifications true` enables action notifications only, `false` disables all notifications, and `all` enables action and clipboard-change notifications. When the option is omitted, `notifications` and `notify_on_change` are read from the configuration file. The tray menu remains a runtime master toggle.

`update_method` accepts `EVENTS` or `POLLING`. `EVENTS` is the default and refreshes only when Wayland reports a primary or regular selection change. If event monitoring is unavailable or disconnects, the applet falls back to `POLLING` and uses `polling_period_ms`. `POLLING` always reads both clipboards at that interval. At idle, `EVENTS` schedules no clipboard polling wakeups; `POLLING` schedules one refresh per configured period.

`icon_name` is a Freedesktop icon-theme name resolved by the desktop, not a Unicode character or image path.

`editor` is an argument list for an external editor. Editing is disabled when the list is empty. The applet executes the program directly without a shell and appends a private temporary-file path as the final argument. The command must wait until editing finishes; graphical editors commonly need a wait option. For example:

```toml
editor = ["foot", "-e", "nvim"]
```

Edited files larger than 1 MiB are rejected, and the original value is preserved when the editor fails or produces invalid UTF-8.

`stack_enabled = false` removes all stack actions and entries from the tray menu. `stack_size` limits the enabled shared in-memory clipboard stack to between 1 and 16 text entries. The stack is cleared when the applet exits.

`max_clipboard_bytes` limits each text value read from Wayland. `max_stack_entry_bytes` limits each stacked or edited stack value. Both limits are measured in UTF-8 bytes and default to 1 MiB. Oversized values are rejected rather than truncated, leaving existing destinations and stack entries unchanged.

Only one applet instance can run in a desktop session. A second process exits with an explanatory error. The advisory lock is released automatically when the running process exits or crashes, so a leftover lock file does not block future starts.

## Tray controls

- Left and middle click perform their configured actions.
- Right click opens the action menu.
- Copy, Stack, Clear, Switch, and Reset entries are disabled when their required clipboard content is empty.
- Edit actions are enabled for text selections and stack entries when `editor` is configured.
- Stack entries are listed newest-first unless `hide_content` is enabled. Pop can target primary, regular, or both; an entry is removed only after the write succeeds.
- When the stack is empty, pop actions are replaced by `No stacked entries yet`.
- `Enable notifications` / `Disable notifications` changes notification behavior for the current process without modifying the configuration file.
- `Exit` shuts down the applet. Wayland clipboard values owned by this process may disappear unless a clipboard manager retains them.

See [contrib/config.toml](contrib/config.toml) for the complete documented configuration.

## References

- [ksni](https://docs.rs/ksni/) — StatusNotifierItem tray integration
- [wl-clipboard-rs](https://docs.rs/wl-clipboard-rs/) — Wayland clipboard access

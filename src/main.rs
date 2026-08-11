mod cli;
mod clipboard;
mod clipboard_monitor;
mod config;
mod editor;
mod notification;
mod stack;
mod tray;

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ksni::TrayMethods;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wl_clipboard_rs::copy::ClipboardType as CopyClipboardType;
use wl_clipboard_rs::paste::ClipboardType;

use cli::{CliAction, parse_args, print_help};
use clipboard::{ClipboardRead, perform_action, perform_clear, read, read_both, write};
use clipboard_monitor::MonitorEvent;
use config::UpdateMethod;
use notification::{clipboard_change, send_change, settings as notification_settings};
use stack::{StackEntry, perform as perform_stack_action};
use tray::{AppEvent, ClipboardTray, EditTarget, tooltip_text};

type EditorResult = Result<(EditTarget, String, String), String>;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let action = parse_args(std::env::args_os().skip(1)).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let CliAction::Run {
        config_file,
        debug,
        with_notifications,
    } = action
    else {
        print_help();
        return;
    };

    let lock_path =
        instance_lock_path(std::env::var_os("XDG_RUNTIME_DIR")).unwrap_or_else(|error| {
            eprintln!("failed to establish single-instance lock: {error}");
            std::process::exit(1);
        });
    let _instance_lock = acquire_instance_lock(&lock_path).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    if debug {
        eprintln!("[debug] acquired instance lock: {}", lock_path.display());
    }

    let config = config::load(config_file.as_deref()).unwrap_or_else(|error| {
        eprintln!("failed to load configuration: {error}");
        std::process::exit(1);
    });
    let polling_period = Duration::from_millis(config.polling_period_ms);
    let (mut notifications_enabled, notify_on_change) =
        notification_settings(&config, with_notifications);
    if debug {
        let config_name = config_file
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "XDG default".into());
        eprintln!(
            "[debug] started: config={}, update_method={}, polling_period_ms={}, icon_name={}, stack_size={}, max_clipboard_bytes={}, max_stack_entry_bytes={}, left_click={}, middle_click={}, notifications={}, notify_on_change={}",
            config_name,
            config.update_method.name(),
            config.polling_period_ms,
            config.icon_name,
            config.stack_size,
            config.max_clipboard_bytes,
            config.max_stack_entry_bytes,
            config.left_click.name(),
            config.middle_click.name(),
            notifications_enabled,
            notify_on_change
        );
    }

    let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
    let (monitor_sender, mut monitor_receiver) = mpsc::channel(1);
    let mut stack = Vec::new();
    let mut previous_clipboards = (None, None);
    let initial_primary = ClipboardRead::Empty;
    let initial_regular = ClipboardRead::Empty;
    let tray = ClipboardTray {
        tooltip: tooltip_text(&initial_primary, &initial_regular, config.hide_content),
        icon_name: config.icon_name.clone(),
        event_sender,
        left_click: config.left_click,
        middle_click: config.middle_click,
        primary: initial_primary,
        regular: initial_regular,
        stack: Vec::new(),
        hide_content: config.hide_content,
        notifications_enabled,
        editor_enabled: !config.editor.is_empty(),
        editor_busy: false,
        stack_enabled: config.stack_enabled,
        max_clipboard_bytes: config.max_clipboard_bytes,
    };
    let handle = tray.spawn().await.unwrap_or_else(|error| {
        eprintln!("failed to register tray icon: {error}");
        std::process::exit(1);
    });

    let mut poll_interval = match config.update_method {
        UpdateMethod::Events => match clipboard_monitor::spawn(monitor_sender) {
            Ok(()) => None,
            Err(error) => {
                eprintln!("{error}; falling back to polling");
                Some(new_poll_interval(polling_period))
            }
        },
        UpdateMethod::Polling => {
            drop(monitor_sender);
            Some(new_poll_interval(polling_period))
        }
    };
    let mut interrupt_signal = signal(SignalKind::interrupt()).unwrap_or_else(|error| {
        eprintln!("failed to install SIGINT handler: {error}");
        std::process::exit(1);
    });
    let mut terminate_signal = signal(SignalKind::terminate()).unwrap_or_else(|error| {
        eprintln!("failed to install SIGTERM handler: {error}");
        std::process::exit(1);
    });
    let mut editor_task: Option<JoinHandle<EditorResult>> = None;

    loop {
        let mut check_for_change = false;
        tokio::select! {
            signal_name = wait_for_shutdown_signal(&mut interrupt_signal, &mut terminate_signal) => {
                if debug { eprintln!("[debug] exit requested: source={signal_name}"); }
                break;
            }
            _ = wait_for_poll(&mut poll_interval) => { check_for_change = true; }
            Some(event) = monitor_receiver.recv() => {
                match event {
                    MonitorEvent::ClipboardChanged => {
                        check_for_change = true;
                        if debug { eprintln!("[debug] clipboard change event received"); }
                    }
                    MonitorEvent::Failed => {
                        if debug { eprintln!("[debug] clipboard update method changed: EVENTS -> POLLING"); }
                        poll_interval = Some(new_poll_interval(polling_period));
                    }
                }
            }
            Some(event) = event_receiver.recv() => {
                match event {
                    AppEvent::Exit => {
                        if debug { eprintln!("[debug] exit requested: source=Menu"); }
                        break;
                    }
                    AppEvent::Action(request) => {
                        if debug {
                            eprintln!("[debug] action requested: action={}, source={:?}", request.action.name(), request.source);
                        }
                        let max_bytes = config.max_clipboard_bytes;
                        let result = tokio::task::spawn_blocking(move || perform_action(request.action, max_bytes, notifications_enabled, debug)).await;
                        match result {
                            Ok(Ok(())) if debug => eprintln!("[debug] action completed: {}", request.action.name()),
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => eprintln!("could not perform clipboard action: {error}"),
                            Err(error) => eprintln!("clipboard action stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::Stack(action) => {
                        let mut current_stack = std::mem::take(&mut stack);
                        let capacity = config.stack_size;
                        let max_clipboard_bytes = config.max_clipboard_bytes;
                        let max_entry_bytes = config.max_stack_entry_bytes;
                        let result = tokio::task::spawn_blocking(move || {
                            let result = perform_stack_action(
                                action,
                                &mut current_stack,
                                capacity,
                                max_clipboard_bytes,
                                max_entry_bytes,
                                notifications_enabled,
                                debug,
                            );
                            (result, current_stack)
                        }).await;
                        match result {
                            Ok((result, returned_stack)) => {
                                stack = returned_stack;
                                if let Err(error) = result { eprintln!("could not perform stack action: {error}"); }
                            }
                            Err(error) => eprintln!("clipboard stack action stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::Clear(target) => {
                        match tokio::task::spawn_blocking(move || perform_clear(target, notifications_enabled, debug)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => eprintln!("could not clear clipboard: {error}"),
                            Err(error) => eprintln!("clipboard clear stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::Edit(target) => {
                        if editor_task.is_some() {
                            eprintln!("could not edit clipboard value: an editor is already running");
                        } else {
                            let command = config.editor.clone();
                            let max_clipboard_bytes = config.max_clipboard_bytes;
                            let max_stack_entry_bytes = config.max_stack_entry_bytes;
                            let stack_value = match target {
                                EditTarget::Stack(id) => stack
                                    .iter()
                                    .find(|entry| entry.id == id)
                                    .map(|entry| entry.value.clone()),
                                EditTarget::Primary | EditTarget::Regular => None,
                            };
                            editor_task = Some(tokio::spawn(run_editor(
                                target,
                                stack_value,
                                command,
                                max_clipboard_bytes,
                                max_stack_entry_bytes,
                                debug,
                            )));
                            if debug { eprintln!("[debug] editor task started: target={target:?}"); }
                        }
                    }
                    AppEvent::ToggleNotifications => {
                        notifications_enabled = !notifications_enabled;
                        if debug { eprintln!("[debug] notifications toggled: enabled={notifications_enabled}"); }
                    }
                }
            }
            result = wait_for_editor(&mut editor_task) => {
                editor_task = None;
                match result {
                    Ok(Ok((target, original, edited))) => {
                        if let Err(error) = apply_edit(
                            target,
                            original,
                            edited,
                            &mut stack,
                            notifications_enabled,
                            debug,
                        ).await {
                            eprintln!("could not edit clipboard value: {error}");
                        }
                    }
                    Ok(Err(error)) => eprintln!("could not edit clipboard value: {error}"),
                    Err(error) => eprintln!("clipboard editor stopped unexpectedly: {error}"),
                }
            }
        }

        let (primary, regular) = match tokio::task::spawn_blocking({
            let max_bytes = config.max_clipboard_bytes;
            move || read_both(max_bytes)
        })
        .await
        {
            Ok(clipboards) => clipboards,
            Err(error) => {
                eprintln!("clipboard reader stopped unexpectedly: {error}");
                let error = error.to_string();
                (
                    ClipboardRead::Error(error.clone()),
                    ClipboardRead::Error(error),
                )
            }
        };
        report_read_issue("primary", &primary);
        report_read_issue("regular", &regular);
        let current_clipboards = (primary.observation(), regular.observation());
        let change = clipboard_change(&mut previous_clipboards, &current_clipboards);
        if check_for_change
            && notify_on_change
            && notifications_enabled
            && let Some(change) = change
        {
            let _ = tokio::task::spawn_blocking(move || send_change(change, debug)).await;
        } else if check_for_change && notify_on_change && debug {
            eprintln!(
                "[debug] clipboard change notification skipped: changed={}, notifications={notifications_enabled}",
                change.is_some()
            );
        }
        let tooltip = tooltip_text(&primary, &regular, config.hide_content);
        handle
            .update(|tray| {
                tray.tooltip = tooltip;
                tray.primary = primary;
                tray.regular = regular;
                tray.stack = stack.clone();
                tray.notifications_enabled = notifications_enabled;
                tray.editor_busy = editor_task.is_some();
            })
            .await;
    }

    if let Some(task) = editor_task.take() {
        task.abort();
        let _ = task.await;
        if debug {
            eprintln!("[debug] editor task cancelled during shutdown");
        }
    }
}

async fn run_editor(
    target: EditTarget,
    stack_value: Option<String>,
    command: Vec<String>,
    max_clipboard_bytes: u64,
    max_stack_entry_bytes: u64,
    debug: bool,
) -> EditorResult {
    if debug {
        eprintln!("[debug] edit requested: target={target:?}");
    }
    let original = match target {
        EditTarget::Primary | EditTarget::Regular => tokio::task::spawn_blocking(move || {
            let (clipboard, name) = match target {
                EditTarget::Primary => (ClipboardType::Primary, "primary"),
                EditTarget::Regular => (ClipboardType::Regular, "regular"),
                EditTarget::Stack(_) => unreachable!(),
            };
            read(clipboard, max_clipboard_bytes).into_editable(name)
        })
        .await
        .map_err(|error| format!("clipboard reader stopped unexpectedly: {error}"))??,
        EditTarget::Stack(_) => {
            stack_value.ok_or_else(|| "stacked entry no longer exists".to_string())?
        }
    };
    let max_edited_bytes = match target {
        EditTarget::Primary | EditTarget::Regular => max_clipboard_bytes,
        EditTarget::Stack(_) => max_stack_entry_bytes,
    };
    let edited = editor::edit(&command, &original, max_edited_bytes, debug).await?;
    Ok((target, original, edited))
}

async fn apply_edit(
    target: EditTarget,
    original: String,
    edited: String,
    stack: &mut [StackEntry],
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    match target {
        EditTarget::Primary | EditTarget::Regular => {
            tokio::task::spawn_blocking(move || {
                let clipboard = match target {
                    EditTarget::Primary => CopyClipboardType::Primary,
                    EditTarget::Regular => CopyClipboardType::Regular,
                    EditTarget::Stack(_) => unreachable!(),
                };
                write(clipboard, edited, debug)
            })
            .await
            .map_err(|error| format!("clipboard writer stopped unexpectedly: {error}"))??;
        }
        EditTarget::Stack(id) => {
            replace_stack_entry_if_unchanged(stack, id, &original, edited)?;
        }
    }
    let sent = send_edit_notification(move || {
        notification::send_if_enabled("Clipboard value edited", "clipboard edit", notifications)
    })
    .await?;
    if sent && debug {
        eprintln!("[debug] edit notification sent: target={target:?}");
    }
    Ok(())
}

async fn send_edit_notification<F>(send: F) -> Result<bool, String>
where
    F: FnOnce() -> bool + Send + 'static,
{
    tokio::task::spawn_blocking(send)
        .await
        .map_err(|error| format!("edit notification worker stopped unexpectedly: {error}"))
}

fn replace_stack_entry_if_unchanged(
    stack: &mut [StackEntry],
    id: u64,
    original: &str,
    value: String,
) -> Result<(), String> {
    let entry = stack
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| "stacked entry no longer exists".to_string())?;
    if entry.value != original {
        return Err("stacked entry changed while it was being edited".into());
    }
    entry.value = value;
    Ok(())
}

async fn wait_for_editor(task: &mut Option<JoinHandle<EditorResult>>) -> EditorResultJoin {
    match task {
        Some(task) => task.await,
        None => std::future::pending().await,
    }
}

type EditorResultJoin = Result<EditorResult, tokio::task::JoinError>;

fn report_read_issue(selection: &str, value: &ClipboardRead) {
    match value {
        ClipboardRead::Oversized { limit } => {
            eprintln!("could not read {selection} clipboard: content exceeds {limit} bytes");
        }
        ClipboardRead::Error(error) => {
            eprintln!("could not read {selection} clipboard: {error}");
        }
        ClipboardRead::Text(_)
        | ClipboardRead::Empty
        | ClipboardRead::NonText
        | ClipboardRead::Unsupported => {}
    }
}

fn new_poll_interval(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

async fn wait_for_poll(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

async fn wait_for_shutdown_signal(interrupt: &mut Signal, terminate: &mut Signal) -> &'static str {
    tokio::select! {
        _ = interrupt.recv() => "SIGINT",
        _ = terminate.recv() => "SIGTERM",
    }
}

fn instance_lock_path(runtime_dir: Option<OsString>) -> Result<PathBuf, String> {
    let runtime_dir = runtime_dir
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())?;
    Ok(PathBuf::from(runtime_dir).join("clipboard-applet.lock"))
}

fn acquire_instance_lock(path: &Path) -> Result<File, String> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open instance lock {}: {error}", path.display()))?;
    // SAFETY: `file` owns a valid descriptor and `flock` does not retain it.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
            format!(
                "{} is already running for this session (lock: {})",
                env!("CARGO_PKG_NAME"),
                path.display()
            )
        } else {
            format!("failed to lock {}: {error}", path.display())
        });
    }
    file.set_len(0)
        .and_then(|()| writeln!(file, "{}", std::process::id()))
        .map_err(|error| format!("failed to write instance lock {}: {error}", path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_test_path() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "clipboard-applet-instance-lock-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn instance_lock_path_uses_xdg_runtime_directory() {
        assert_eq!(
            instance_lock_path(Some(OsString::from("/run/user/1000"))),
            Ok(PathBuf::from("/run/user/1000/clipboard-applet.lock"))
        );
        assert!(instance_lock_path(None).is_err());
    }

    #[test]
    fn instance_lock_rejects_contention_and_can_be_reacquired() {
        let path = unique_test_path();
        let first = acquire_instance_lock(&path).unwrap();
        assert!(
            acquire_instance_lock(&path)
                .unwrap_err()
                .contains("already running")
        );
        drop(first);
        drop(acquire_instance_lock(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn stack_edit_uses_stable_identity_with_duplicate_values() {
        let first = StackEntry::new("duplicate".into());
        let target = StackEntry::new("duplicate".into());
        let last = StackEntry::new("duplicate".into());
        let target_id = target.id;
        let mut stack = vec![first, target, last];

        stack.insert(0, StackEntry::new("pushed".into()));
        replace_stack_entry_if_unchanged(&mut stack, target_id, "duplicate", "edited".into())
            .unwrap();
        assert_eq!(
            stack
                .iter()
                .filter(|entry| entry.value == "edited")
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [target_id]
        );

        let removed = stack.remove(
            stack
                .iter()
                .position(|entry| entry.id == target_id)
                .unwrap(),
        );
        assert!(
            replace_stack_entry_if_unchanged(&mut stack, removed.id, "edited", "stale".into())
                .is_err()
        );
        assert!(stack.iter().all(|entry| entry.value != "stale"));
    }

    #[test]
    fn stack_edit_rejects_targets_removed_by_pop_or_eviction() {
        for remove_target in [true, false] {
            let target = StackEntry::new("target".into());
            let target_id = target.id;
            let mut stack = if remove_target {
                vec![StackEntry::new("kept".into()), target]
            } else {
                vec![target, StackEntry::new("kept".into())]
            };
            if remove_target {
                stack.pop();
            } else {
                stack.remove(0);
            }
            assert!(
                replace_stack_entry_if_unchanged(&mut stack, target_id, "target", "edited".into())
                    .is_err()
            );
            assert_eq!(stack[0].value, "kept");
        }
    }
}

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
use wl_clipboard_rs::copy::ClipboardType as CopyClipboardType;
use wl_clipboard_rs::paste::ClipboardType;

use cli::{CliAction, parse_args, print_help};
use clipboard::{perform_action, perform_clear, read, try_read_both, write};
use clipboard_monitor::MonitorEvent;
use config::UpdateMethod;
use notification::{clipboard_change, send_change, settings as notification_settings};
use stack::perform as perform_stack_action;
use tray::{AppEvent, ClipboardTray, EditTarget, tooltip_text};

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
            "[debug] started: config={}, update_method={}, polling_period_ms={}, icon_name={}, stack_size={}, left_click={}, middle_click={}, notifications={}, notify_on_change={}",
            config_name,
            config.update_method.name(),
            config.polling_period_ms,
            config.icon_name,
            config.stack_size,
            config.left_click.name(),
            config.middle_click.name(),
            notifications_enabled,
            notify_on_change
        );
    }

    let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
    let (monitor_sender, mut monitor_receiver) = mpsc::channel(1);
    let mut stack = Vec::new();
    let mut previous_clipboards = None;
    let tray = ClipboardTray {
        tooltip: tooltip_text(None, None, config.hide_content),
        icon_name: config.icon_name.clone(),
        event_sender,
        left_click: config.left_click,
        middle_click: config.middle_click,
        primary: None,
        regular: None,
        stack: Vec::new(),
        hide_content: config.hide_content,
        notifications_enabled,
        editor_enabled: !config.editor.is_empty(),
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
                        let result = tokio::task::spawn_blocking(move || perform_action(request.action, notifications_enabled, debug)).await;
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
                        let result = tokio::task::spawn_blocking(move || {
                            let result = perform_stack_action(action, &mut current_stack, capacity, notifications_enabled, debug);
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
                        let command = config.editor.clone();
                        let mut current_stack = std::mem::take(&mut stack);
                        let result = tokio::task::spawn_blocking(move || {
                            let result = perform_edit(
                                target,
                                &mut current_stack,
                                &command,
                                notifications_enabled,
                                debug,
                            );
                            (result, current_stack)
                        }).await;
                        match result {
                            Ok((result, returned_stack)) => {
                                stack = returned_stack;
                                if let Err(error) = result { eprintln!("could not edit clipboard value: {error}"); }
                            }
                            Err(error) => eprintln!("clipboard editor stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::ToggleNotifications => {
                        notifications_enabled = !notifications_enabled;
                        if debug { eprintln!("[debug] notifications toggled: enabled={notifications_enabled}"); }
                    }
                }
            }
        }

        let ((primary, regular), read_succeeded) =
            match tokio::task::spawn_blocking(try_read_both).await {
                Ok(Ok(clipboards)) => (clipboards, true),
                Ok(Err(error)) => {
                    eprintln!("could not read clipboards: {error}");
                    ((None, None), false)
                }
                Err(error) => {
                    eprintln!("clipboard reader stopped unexpectedly: {error}");
                    ((None, None), false)
                }
            };
        let current_clipboards = (primary.clone(), regular.clone());
        let change = read_succeeded
            .then(|| {
                previous_clipboards
                    .as_ref()
                    .and_then(|previous| clipboard_change(previous, &current_clipboards))
            })
            .flatten();
        if read_succeeded {
            previous_clipboards = Some(current_clipboards);
        }
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
        let tooltip = tooltip_text(primary.as_deref(), regular.as_deref(), config.hide_content);
        handle
            .update(|tray| {
                tray.tooltip = tooltip;
                tray.primary = primary;
                tray.regular = regular;
                tray.stack = stack.clone();
                tray.notifications_enabled = notifications_enabled;
            })
            .await;
    }
}

fn perform_edit(
    target: EditTarget,
    stack: &mut [String],
    command: &[String],
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    if debug {
        eprintln!("[debug] edit requested: target={target:?}");
    }
    let original = match target {
        EditTarget::Primary => read(ClipboardType::Primary).into_editable("primary")?,
        EditTarget::Regular => read(ClipboardType::Regular).into_editable("regular")?,
        EditTarget::Stack(index) => stack
            .get(index)
            .cloned()
            .ok_or_else(|| "stacked entry no longer exists".to_string())?,
    };
    let edited = editor::edit(command, &original, debug)?;
    match target {
        EditTarget::Primary => write(CopyClipboardType::Primary, edited, debug)?,
        EditTarget::Regular => write(CopyClipboardType::Regular, edited, debug)?,
        EditTarget::Stack(index) => replace_stack_entry(stack, index, edited)?,
    }
    let sent =
        notification::send_if_enabled("Clipboard value edited", "clipboard edit", notifications);
    if sent && debug {
        eprintln!("[debug] edit notification sent: target={target:?}");
    }
    Ok(())
}

fn replace_stack_entry(stack: &mut [String], index: usize, value: String) -> Result<(), String> {
    let entry = stack
        .get_mut(index)
        .ok_or_else(|| "stacked entry no longer exists".to_string())?;
    *entry = value;
    Ok(())
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
    fn stack_edit_replaces_in_place() {
        let mut stack = vec!["oldest".into(), "target".into(), "newest".into()];
        replace_stack_entry(&mut stack, 1, "edited".into()).unwrap();
        assert_eq!(stack, ["oldest", "edited", "newest"]);
        assert!(replace_stack_entry(&mut stack, 3, "missing".into()).is_err());
    }
}

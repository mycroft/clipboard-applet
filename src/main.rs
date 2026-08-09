mod clipboard_monitor;

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ksni::{ToolTip, TrayMethods};
use serde::Deserialize;
use tokio::sync::mpsc;
use wl_clipboard_rs::copy::{
    ClipboardType as CopyClipboardType, MimeType as CopyMimeType, Options, Seat as CopySeat,
    Source, clear,
};
use wl_clipboard_rs::paste::{ClipboardType, Error, MimeType, Seat, get_contents};

use clipboard_monitor::MonitorEvent;

const PREVIEW_CHARS: usize = 40;
const DEFAULT_POLLING_PERIOD_MS: u64 = 1_000;

#[derive(Debug, Eq, PartialEq)]
enum CliAction {
    Run {
        config_file: Option<PathBuf>,
        debug: bool,
        with_notifications: Option<NotificationMode>,
    },
    Help,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NotificationMode {
    Disabled,
    Enabled,
    All,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ClipboardAction {
    CopyRegular,
    CopyPrimary,
    Reset,
    Switch,
}

impl ClipboardAction {
    fn name(self) -> &'static str {
        match self {
            Self::CopyRegular => "COPY_REGULAR",
            Self::CopyPrimary => "COPY_PRIMARY",
            Self::Reset => "RESET",
            Self::Switch => "SWITCH",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum UpdateMethod {
    Events,
    Polling,
}

impl UpdateMethod {
    fn name(self) -> &'static str {
        match self {
            Self::Events => "EVENTS",
            Self::Polling => "POLLING",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionSource {
    LeftClick,
    MiddleClick,
    Menu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActionRequest {
    action: ClipboardAction,
    source: ActionSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StackAction {
    PushPrimary,
    PushRegular,
    PopPrimary,
    PopRegular,
    PopBoth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClearTarget {
    Primary,
    Regular,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClipboardChange {
    Primary,
    Regular,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppEvent {
    Action(ActionRequest),
    Stack(StackAction),
    Clear(ClearTarget),
    ToggleNotifications,
    Exit,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    update_method: UpdateMethod,
    polling_period_ms: u64,
    hide_content: bool,
    notifications: bool,
    notify_on_change: bool,
    icon_name: String,
    stack_size: usize,
    left_click: ClipboardAction,
    middle_click: ClipboardAction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            update_method: UpdateMethod::Events,
            polling_period_ms: DEFAULT_POLLING_PERIOD_MS,
            hide_content: false,
            notifications: false,
            notify_on_change: false,
            icon_name: "edit-paste".into(),
            stack_size: 16,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
        }
    }
}

#[derive(Debug)]
struct ClipboardTray {
    tooltip: String,
    icon_name: String,
    event_sender: mpsc::UnboundedSender<AppEvent>,
    left_click: ClipboardAction,
    middle_click: ClipboardAction,
    primary: Option<String>,
    regular: Option<String>,
    stack: Vec<String>,
    hide_content: bool,
    notifications_enabled: bool,
}

impl ksni::Tray for ClipboardTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "Clipboard".into()
    }

    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Clipboard".into(),
            description: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.event_sender.send(AppEvent::Action(ActionRequest {
            action: self.left_click,
            source: ActionSource::LeftClick,
        }));
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let _ = self.event_sender.send(AppEvent::Action(ActionRequest {
            action: self.middle_click,
            source: ActionSource::MiddleClick,
        }));
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;

        let preview_item = |name: &str, value: Option<&str>| {
            StandardItem {
                label: menu_label(name, value, self.hide_content),
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        let action_item = |label: &str, action: ClipboardAction, enabled: bool| {
            StandardItem {
                label: label.into(),
                enabled,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::Action(ActionRequest {
                        action,
                        source: ActionSource::Menu,
                    }));
                }),
                ..Default::default()
            }
            .into()
        };
        let separator_item = || {
            StandardItem {
                label: "────────────────────".into(),
                enabled: false,
                ..Default::default()
            }
            .into()
        };
        let stack_action_item = |label: &str, action: StackAction, enabled: bool| {
            StandardItem {
                label: label.into(),
                enabled,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::Stack(action));
                }),
                ..Default::default()
            }
            .into()
        };
        let clear_item = |label: &str, target: ClearTarget, enabled: bool| {
            StandardItem {
                label: label.into(),
                enabled,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::Clear(target));
                }),
                ..Default::default()
            }
            .into()
        };
        let has_clipboard_content =
            has_content(self.primary.as_deref()) || has_content(self.regular.as_deref());

        let mut menu = vec![
            preview_item("Primary", self.primary.as_deref()),
            action_item(
                "Copy primary to regular",
                ClipboardAction::CopyPrimary,
                has_content(self.primary.as_deref()),
            ),
            stack_action_item(
                "Stack primary",
                StackAction::PushPrimary,
                has_content(self.primary.as_deref()),
            ),
            clear_item(
                "Clear primary clipboard",
                ClearTarget::Primary,
                has_content(self.primary.as_deref()),
            ),
            separator_item(),
            preview_item("Regular", self.regular.as_deref()),
            action_item(
                "Copy regular to primary",
                ClipboardAction::CopyRegular,
                has_content(self.regular.as_deref()),
            ),
            stack_action_item(
                "Stack regular",
                StackAction::PushRegular,
                has_content(self.regular.as_deref()),
            ),
            clear_item(
                "Clear regular clipboard",
                ClearTarget::Regular,
                has_content(self.regular.as_deref()),
            ),
            separator_item(),
            action_item(
                "Switch clipboards",
                ClipboardAction::Switch,
                has_clipboard_content,
            ),
            action_item(
                "Reset clipboards",
                ClipboardAction::Reset,
                has_clipboard_content,
            ),
            separator_item(),
        ];
        if self.stack.is_empty() {
            menu.push(
                StandardItem {
                    label: "No stacked entries yet".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            if !self.hide_content {
                menu.extend(self.stack.iter().rev().enumerate().map(|(index, value)| {
                    StandardItem {
                        label: format!("{}: {}", index + 1, preview(Some(value), false))
                            .replace('_', "__"),
                        enabled: false,
                        ..Default::default()
                    }
                    .into()
                }));
            }
            menu.extend([
                stack_action_item("Pop to primary", StackAction::PopPrimary, true),
                stack_action_item("Pop to regular", StackAction::PopRegular, true),
                stack_action_item("Pop to primary and regular", StackAction::PopBoth, true),
            ]);
        }
        menu.extend([
            separator_item(),
            StandardItem {
                label: if self.notifications_enabled {
                    "Disable notifications"
                } else {
                    "Enable notifications"
                }
                .into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::ToggleNotifications);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Exit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::Exit);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        menu
    }

    fn menu_about_to_show(&mut self) {
        (self.primary, self.regular) = read_clipboards();
    }
}

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
    let config = load_config(config_file.as_deref()).unwrap_or_else(|error| {
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
    loop {
        let mut check_for_change = false;
        tokio::select! {
            _ = wait_for_poll(&mut poll_interval) => {
                check_for_change = true;
            }
            Some(event) = monitor_receiver.recv() => {
                match event {
                    MonitorEvent::ClipboardChanged => {
                        check_for_change = true;
                        if debug {
                            eprintln!("[debug] clipboard change event received");
                        }
                    }
                    MonitorEvent::Failed => {
                        if debug {
                            eprintln!("[debug] clipboard update method changed: EVENTS -> POLLING");
                        }
                        poll_interval = Some(new_poll_interval(polling_period));
                    }
                }
            }
            Some(event) = event_receiver.recv() => {
                match event {
                    AppEvent::Exit => {
                        if debug {
                            eprintln!("[debug] exit requested: source=Menu");
                        }
                        break;
                    }
                    AppEvent::Action(request) => {
                        let notifications = notifications_enabled;
                        if debug {
                            eprintln!(
                                "[debug] action requested: action={}, source={:?}",
                                request.action.name(), request.source
                            );
                        }
                        match tokio::task::spawn_blocking(move || perform_action(request.action, notifications, debug)).await {
                            Ok(Ok(())) => {
                                if debug {
                                    eprintln!("[debug] action completed: {}", request.action.name());
                                }
                            }
                            Ok(Err(error)) => eprintln!("could not perform clipboard action: {error}"),
                            Err(error) => eprintln!("clipboard action stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::Stack(action) => {
                        let notifications = notifications_enabled;
                        let capacity = config.stack_size;
                        let mut current_stack = std::mem::take(&mut stack);
                        let result = tokio::task::spawn_blocking(move || {
                            let result = perform_stack_action(
                                action,
                                &mut current_stack,
                                capacity,
                                notifications,
                                debug,
                            );
                            (result, current_stack)
                        })
                        .await;
                        match result {
                            Ok((result, returned_stack)) => {
                                stack = returned_stack;
                                if let Err(error) = result {
                                    eprintln!("could not perform stack action: {error}");
                                }
                            }
                            Err(error) => eprintln!("clipboard stack action stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::Clear(target) => {
                        let notifications = notifications_enabled;
                        match tokio::task::spawn_blocking(move || {
                            perform_clear(target, notifications, debug)
                        })
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => eprintln!("could not clear clipboard: {error}"),
                            Err(error) => eprintln!("clipboard clear stopped unexpectedly: {error}"),
                        }
                    }
                    AppEvent::ToggleNotifications => {
                        notifications_enabled = !notifications_enabled;
                        if debug {
                            eprintln!(
                                "[debug] notifications toggled: enabled={notifications_enabled}"
                            );
                        }
                    }
                }
            }
        }

        let ((primary, regular), read_succeeded) =
            match tokio::task::spawn_blocking(try_read_clipboards).await {
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
        let change = if read_succeeded {
            previous_clipboards
                .as_ref()
                .and_then(|previous| clipboard_change(previous, &current_clipboards))
        } else {
            None
        };
        if read_succeeded {
            previous_clipboards = Some(current_clipboards);
        }
        if check_for_change
            && notify_on_change
            && notifications_enabled
            && let Some(change) = change
        {
            let _ =
                tokio::task::spawn_blocking(move || send_change_notification(change, debug)).await;
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

fn clipboard_change(
    previous: &(Option<String>, Option<String>),
    current: &(Option<String>, Option<String>),
) -> Option<ClipboardChange> {
    match (previous.0 != current.0, previous.1 != current.1) {
        (true, true) => Some(ClipboardChange::Both),
        (true, false) => Some(ClipboardChange::Primary),
        (false, true) => Some(ClipboardChange::Regular),
        (false, false) => None,
    }
}

fn send_change_notification(change: ClipboardChange, debug: bool) {
    let body = match change {
        ClipboardChange::Primary => "Primary clipboard changed",
        ClipboardChange::Regular => "Regular clipboard changed",
        ClipboardChange::Both => "Primary and regular clipboards changed",
    };
    if let Err(error) = notify_rust::Notification::new()
        .summary("Clipboard applet")
        .body(body)
        .icon("edit-paste")
        .show()
    {
        eprintln!("could not send clipboard change notification: {error}");
    } else if debug {
        eprintln!("[debug] clipboard change notification sent: {change:?}");
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

    // SAFETY: `file` owns a valid descriptor for this call, and `flock` does
    // not retain the descriptor or dereference a Rust pointer.
    let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if lock_result == -1 {
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

fn print_help() {
    println!(
        "{name} {version}\n\nWayland clipboard tray applet\n\nUsage: {name} [OPTIONS]\n\nOptions:\n  -c, --config-file <PATH>            Use this configuration file\n  -d, --debug                         Log clipboard actions to stderr\n      --with-notifications <MODE>     Notification mode: true, false, or all\n  -h, --help                          Show this help",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION")
    );
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<CliAction, String> {
    let mut args = args.into_iter();
    let mut config_file = None;
    let mut debug = false;
    let mut with_notifications = None;
    while let Some(argument) = args.next() {
        let value = match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-d" | "--debug") => {
                debug = true;
                continue;
            }
            Some("--with-notifications") => {
                let value = args.next().ok_or_else(|| {
                    "--with-notifications requires true, false, or all".to_string()
                })?;
                let value = value.to_str().ok_or_else(|| {
                    "--with-notifications requires true, false, or all".to_string()
                })?;
                if with_notifications
                    .replace(parse_notification_mode(value)?)
                    .is_some()
                {
                    return Err("--with-notifications specified more than once".into());
                }
                continue;
            }
            Some(argument) if argument.starts_with("--with-notifications=") => {
                let value = &argument["--with-notifications=".len()..];
                if with_notifications
                    .replace(parse_notification_mode(value)?)
                    .is_some()
                {
                    return Err("--with-notifications specified more than once".into());
                }
                continue;
            }
            Some("-c" | "--config-file") => PathBuf::from(
                args.next()
                    .ok_or_else(|| format!("{} requires a path", argument.to_string_lossy()))?,
            ),
            Some(argument) if argument.starts_with("--config-file=") => {
                let path = &argument["--config-file=".len()..];
                if path.is_empty() {
                    return Err("--config-file requires a path".into());
                }
                path.into()
            }
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy())),
        };
        if config_file.replace(value).is_some() {
            return Err("configuration file specified more than once".into());
        }
    }
    Ok(CliAction::Run {
        config_file,
        debug,
        with_notifications,
    })
}

fn parse_notification_mode(value: &str) -> Result<NotificationMode, String> {
    match value {
        "true" => Ok(NotificationMode::Enabled),
        "false" => Ok(NotificationMode::Disabled),
        "all" => Ok(NotificationMode::All),
        _ => Err(format!(
            "invalid --with-notifications value {value:?}; expected true, false, or all"
        )),
    }
}

fn notification_settings(config: &Config, override_mode: Option<NotificationMode>) -> (bool, bool) {
    match override_mode {
        None => (config.notifications, config.notify_on_change),
        Some(NotificationMode::Disabled) => (false, false),
        Some(NotificationMode::Enabled) => (true, false),
        Some(NotificationMode::All) => (true, true),
    }
}

fn load_config(config_file: Option<&Path>) -> Result<Config, String> {
    if let Some(path) = config_file {
        return load_config_from(path, false);
    }
    let Some(path) = config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    ) else {
        return Ok(Config::default());
    };
    load_config_from(&path, true)
}

fn config_path(
    xdg_config_home: Option<impl Into<PathBuf>>,
    home: Option<impl Into<PathBuf>>,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(Into::into)
        .or_else(|| home.map(|path| path.into().join(".config")))?;
    Some(base.join("clipboard-applet/config.toml"))
}

fn load_config_from(path: &Path, use_default_if_missing: bool) -> Result<Config, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if use_default_if_missing && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    parse_config(&contents, path)
}

fn parse_config(contents: &str, path: &Path) -> Result<Config, String> {
    let config: Config =
        toml::from_str(contents).map_err(|error| format!("{}: {error}", path.display()))?;
    if config.polling_period_ms == 0 {
        return Err(format!(
            "{}: polling_period_ms must be greater than zero",
            path.display()
        ));
    }
    if config.icon_name.trim().is_empty() {
        return Err(format!("{}: icon_name must not be empty", path.display()));
    }
    if !(1..=16).contains(&config.stack_size) {
        return Err(format!(
            "{}: stack_size must be between 1 and 16",
            path.display()
        ));
    }
    Ok(config)
}

fn read_clipboards() -> (Option<String>, Option<String>) {
    try_read_clipboards().unwrap_or_else(|error| {
        eprintln!("could not read clipboards: {error}");
        (None, None)
    })
}

fn try_read_clipboards() -> Result<(Option<String>, Option<String>), String> {
    Ok((
        try_read_clipboard(ClipboardType::Primary)?,
        try_read_clipboard(ClipboardType::Regular)?,
    ))
}

fn try_read_clipboard(clipboard: ClipboardType) -> Result<Option<String>, String> {
    let (mut pipe, _) = match get_contents(clipboard, Seat::Unspecified, MimeType::Text) {
        Ok(contents) => contents,
        Err(Error::ClipboardEmpty | Error::NoMimeType | Error::PrimarySelectionUnsupported) => {
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };

    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)
        .map_err(|error| format!("could not receive contents: {error}"))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn swap_clipboards(debug: bool) -> Result<(), String> {
    let primary = try_read_clipboard(ClipboardType::Primary)?;
    let regular = try_read_clipboard(ClipboardType::Regular)?;
    if debug {
        eprintln!(
            "[debug] SWITCH read: primary={} chars, regular={} chars",
            clipboard_length(primary.as_deref()),
            clipboard_length(regular.as_deref())
        );
    }
    write_clipboard(CopyClipboardType::Primary, regular, debug)?;
    write_clipboard(CopyClipboardType::Regular, primary, debug)
}

fn perform_action(action: ClipboardAction, notifications: bool, debug: bool) -> Result<(), String> {
    match action {
        ClipboardAction::CopyRegular => {
            let regular = try_read_clipboard(ClipboardType::Regular)?;
            if debug {
                eprintln!(
                    "[debug] COPY_REGULAR read: regular={} chars",
                    clipboard_length(regular.as_deref())
                );
            }
            write_clipboard(CopyClipboardType::Primary, regular, debug)?;
        }
        ClipboardAction::CopyPrimary => {
            let primary = try_read_clipboard(ClipboardType::Primary)?;
            if debug {
                eprintln!(
                    "[debug] COPY_PRIMARY read: primary={} chars",
                    clipboard_length(primary.as_deref())
                );
            }
            write_clipboard(CopyClipboardType::Regular, primary, debug)?;
        }
        ClipboardAction::Reset => {
            if debug {
                eprintln!("[debug] RESET clearing primary and regular clipboards");
            }
            clear(CopyClipboardType::Both, CopySeat::All).map_err(|error| error.to_string())?;
        }
        ClipboardAction::Switch => swap_clipboards(debug)?,
    }
    if notifications
        && let Err(error) = notify_rust::Notification::new()
            .summary("Clipboard applet")
            .body(action_notification(action))
            .icon("edit-paste")
            .show()
    {
        eprintln!("could not send clipboard notification: {error}");
    } else if notifications && debug {
        eprintln!("[debug] notification sent: {}", action.name());
    } else if debug {
        eprintln!("[debug] notification skipped: disabled");
    }
    Ok(())
}

fn action_notification(action: ClipboardAction) -> &'static str {
    match action {
        ClipboardAction::CopyRegular => "Regular clipboard copied to primary",
        ClipboardAction::CopyPrimary => "Primary clipboard copied to regular",
        ClipboardAction::Reset => "Primary and regular clipboards cleared",
        ClipboardAction::Switch => "Primary and regular clipboards switched",
    }
}

fn perform_clear(target: ClearTarget, notifications: bool, debug: bool) -> Result<(), String> {
    let clipboard = match target {
        ClearTarget::Primary => CopyClipboardType::Primary,
        ClearTarget::Regular => CopyClipboardType::Regular,
    };
    if debug {
        eprintln!("[debug] clearing clipboard from menu: {clipboard:?}");
    }
    clear(clipboard, CopySeat::All).map_err(|error| error.to_string())?;

    if notifications
        && let Err(error) = notify_rust::Notification::new()
            .summary("Clipboard applet")
            .body(match target {
                ClearTarget::Primary => "Primary clipboard cleared",
                ClearTarget::Regular => "Regular clipboard cleared",
            })
            .icon("edit-paste")
            .show()
    {
        eprintln!("could not send clipboard clear notification: {error}");
    } else if notifications && debug {
        eprintln!("[debug] clear notification sent: {target:?}");
    } else if debug {
        eprintln!("[debug] clear notification skipped: disabled");
    }
    Ok(())
}

fn perform_stack_action(
    action: StackAction,
    stack: &mut Vec<String>,
    capacity: usize,
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    match action {
        StackAction::PushPrimary | StackAction::PushRegular => {
            let clipboard = if action == StackAction::PushPrimary {
                ClipboardType::Primary
            } else {
                ClipboardType::Regular
            };
            let value = try_read_clipboard(clipboard)?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "source clipboard is empty".to_string())?;
            if debug {
                eprintln!(
                    "[debug] stack push: source={clipboard:?}, length={} chars",
                    value.chars().count()
                );
            }
            push_stack(stack, value, capacity);
        }
        StackAction::PopPrimary | StackAction::PopRegular => {
            let value = stack
                .last()
                .cloned()
                .ok_or_else(|| "clipboard stack is empty".to_string())?;
            let clipboard = if action == StackAction::PopPrimary {
                CopyClipboardType::Primary
            } else {
                CopyClipboardType::Regular
            };
            if debug {
                eprintln!(
                    "[debug] stack pop: destination={clipboard:?}, length={} chars",
                    value.chars().count()
                );
            }
            write_clipboard(clipboard, Some(value), debug)?;
            stack.pop();
        }
        StackAction::PopBoth => {
            let value = stack
                .last()
                .cloned()
                .ok_or_else(|| "clipboard stack is empty".to_string())?;
            if debug {
                eprintln!(
                    "[debug] stack pop: destination=Primary+Regular, length={} chars",
                    value.chars().count()
                );
            }
            write_clipboard(CopyClipboardType::Primary, Some(value.clone()), debug)?;
            write_clipboard(CopyClipboardType::Regular, Some(value), debug)?;
            stack.pop();
        }
    }

    if notifications
        && let Err(error) = notify_rust::Notification::new()
            .summary("Clipboard applet")
            .body(stack_action_notification(action))
            .icon("edit-paste")
            .show()
    {
        eprintln!("could not send clipboard stack notification: {error}");
    } else if notifications && debug {
        eprintln!("[debug] stack notification sent: {action:?}");
    } else if debug {
        eprintln!("[debug] stack notification skipped: disabled");
    }
    Ok(())
}

fn push_stack(stack: &mut Vec<String>, value: String, capacity: usize) {
    if stack.len() == capacity {
        stack.remove(0);
    }
    stack.push(value);
}

fn stack_action_notification(action: StackAction) -> &'static str {
    match action {
        StackAction::PushPrimary => "Primary clipboard stacked",
        StackAction::PushRegular => "Regular clipboard stacked",
        StackAction::PopPrimary => "Stacked entry popped to primary",
        StackAction::PopRegular => "Stacked entry popped to regular",
        StackAction::PopBoth => "Stacked entry popped to primary and regular",
    }
}

fn clipboard_length(value: Option<&str>) -> usize {
    value.map_or(0, |value| value.chars().count())
}

fn write_clipboard(
    clipboard: CopyClipboardType,
    value: Option<String>,
    debug: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        if debug {
            eprintln!("[debug] clearing destination: {clipboard:?}");
        }
        return clear(clipboard, CopySeat::All).map_err(|error| error.to_string());
    };
    if debug {
        eprintln!(
            "[debug] writing destination: {clipboard:?}, length={} chars",
            value.chars().count()
        );
    }
    let mut options = Options::new();
    options.clipboard(clipboard);
    options
        .copy(Source::Bytes(value.into_bytes().into()), CopyMimeType::Text)
        .map_err(|error| error.to_string())
}

fn tooltip_text(primary: Option<&str>, regular: Option<&str>, hide_content: bool) -> String {
    format!(
        "primary: {}\nregular: {}",
        preview(primary, hide_content),
        preview(regular, hide_content)
    )
}

fn menu_label(name: &str, value: Option<&str>, hide_content: bool) -> String {
    format!("{name}: {}", preview(value, hide_content)).replace('_', "__")
}

fn has_content(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn preview(value: Option<&str>, hide_content: bool) -> String {
    if hide_content {
        let length = value.map_or(0, |value| value.chars().count());
        return format!("{length} {}", if length == 1 { "char" } else { "chars" });
    }
    let Some(value) = value else {
        return "(empty)".into();
    };
    let value = value.replace(['\n', '\r', '\t'], " ");
    let total = value.chars().count();
    if total <= PREVIEW_CHARS {
        return value;
    }

    let visible: String = value.chars().take(PREVIEW_CHARS).collect();
    format!("{visible}... (and {} more chars)", total - PREVIEW_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_test_path(name: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "clipboard-applet-{name}-{}-{}",
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
        let path = unique_test_path("instance-lock");
        let first = acquire_instance_lock(&path).unwrap();
        let error = acquire_instance_lock(&path).unwrap_err();
        assert!(error.contains("already running"));

        drop(first);
        let second = acquire_instance_lock(&path).unwrap();
        drop(second);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn config_file_short_option_is_parsed() {
        assert_eq!(
            parse_args([OsString::from("-c"), OsString::from("custom.toml")]),
            Ok(CliAction::Run {
                config_file: Some(PathBuf::from("custom.toml")),
                debug: false,
                with_notifications: None,
            })
        );
    }

    #[test]
    fn config_file_long_options_are_parsed() {
        assert_eq!(
            parse_args([OsString::from("--config-file=custom.toml")]),
            Ok(CliAction::Run {
                config_file: Some(PathBuf::from("custom.toml")),
                debug: false,
                with_notifications: None,
            })
        );
        assert_eq!(
            parse_args([
                OsString::from("--config-file"),
                OsString::from("custom.toml")
            ]),
            Ok(CliAction::Run {
                config_file: Some(PathBuf::from("custom.toml")),
                debug: false,
                with_notifications: None,
            })
        );
    }

    #[test]
    fn help_options_are_parsed() {
        assert_eq!(parse_args([OsString::from("-h")]), Ok(CliAction::Help));
        assert_eq!(parse_args([OsString::from("--help")]), Ok(CliAction::Help));
    }

    #[test]
    fn debug_options_are_parsed() {
        assert_eq!(
            parse_args([OsString::from("-d")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: true,
                with_notifications: None,
            })
        );
        assert_eq!(
            parse_args([OsString::from("--debug")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: true,
                with_notifications: None,
            })
        );
    }

    #[test]
    fn notifications_option_is_parsed() {
        assert_eq!(
            parse_args([
                OsString::from("--with-notifications"),
                OsString::from("true")
            ]),
            Ok(CliAction::Run {
                config_file: None,
                debug: false,
                with_notifications: Some(NotificationMode::Enabled),
            })
        );
        assert_eq!(
            parse_args([OsString::from("--with-notifications=false")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: false,
                with_notifications: Some(NotificationMode::Disabled),
            })
        );
        assert_eq!(
            parse_args([OsString::from("--with-notifications=all")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: false,
                with_notifications: Some(NotificationMode::All),
            })
        );
    }

    #[test]
    fn notifications_option_rejects_missing_invalid_and_duplicate_values() {
        assert!(parse_args([OsString::from("--with-notifications")]).is_err());
        assert!(parse_args([OsString::from("--with-notifications=maybe")]).is_err());
        assert!(
            parse_args([
                OsString::from("--with-notifications=true"),
                OsString::from("--with-notifications=all")
            ])
            .is_err()
        );
    }

    #[test]
    fn notification_cli_mode_overrides_configuration() {
        let mut config = Config {
            notifications: true,
            notify_on_change: true,
            ..Config::default()
        };
        assert_eq!(notification_settings(&config, None), (true, true));
        assert_eq!(
            notification_settings(&config, Some(NotificationMode::Disabled)),
            (false, false)
        );
        assert_eq!(
            notification_settings(&config, Some(NotificationMode::Enabled)),
            (true, false)
        );

        config.notifications = false;
        config.notify_on_change = false;
        assert_eq!(
            notification_settings(&config, Some(NotificationMode::All)),
            (true, true)
        );
    }

    #[test]
    fn config_file_option_requires_one_path() {
        assert!(parse_args([OsString::from("-c")]).is_err());
        assert!(parse_args([OsString::from("--config-file=")]).is_err());
        assert!(
            parse_args([
                OsString::from("-c"),
                OsString::from("one.toml"),
                OsString::from("-c"),
                OsString::from("two.toml")
            ])
            .is_err()
        );
    }

    #[test]
    fn config_uses_xdg_path_when_available() {
        assert_eq!(
            config_path(Some("/xdg"), Some("/home/user")),
            Some(PathBuf::from("/xdg/clipboard-applet/config.toml"))
        );
    }

    #[test]
    fn config_falls_back_to_home() {
        assert_eq!(
            config_path(None::<&str>, Some("/home/user")),
            Some(PathBuf::from(
                "/home/user/.config/clipboard-applet/config.toml"
            ))
        );
    }

    #[test]
    fn config_parses_polling_period() {
        let config = parse_config("polling_period_ms = 250", Path::new("config.toml")).unwrap();
        assert_eq!(config.polling_period_ms, 250);
        assert_eq!(config.update_method, UpdateMethod::Events);
        assert!(!config.hide_content);
        assert!(!config.notifications);
        assert!(!config.notify_on_change);
        assert_eq!(config.icon_name, "edit-paste");
        assert_eq!(config.stack_size, 16);
        assert_eq!(config.left_click, ClipboardAction::CopyPrimary);
        assert_eq!(config.middle_click, ClipboardAction::Switch);
    }

    #[test]
    fn config_parses_update_methods() {
        let events = parse_config("update_method = \"EVENTS\"", Path::new("config.toml")).unwrap();
        assert_eq!(events.update_method, UpdateMethod::Events);

        let polling =
            parse_config("update_method = \"POLLING\"", Path::new("config.toml")).unwrap();
        assert_eq!(polling.update_method, UpdateMethod::Polling);
    }

    #[test]
    fn config_parses_hidden_content_setting() {
        let config = parse_config(
            "polling_period_ms = 250\nhide_content = true",
            Path::new("config.toml"),
        )
        .unwrap();
        assert!(config.hide_content);
    }

    #[test]
    fn config_parses_notifications_setting() {
        let config = parse_config(
            "polling_period_ms = 250\nnotifications = true",
            Path::new("config.toml"),
        )
        .unwrap();
        assert!(config.notifications);
    }

    #[test]
    fn config_parses_change_notification_setting() {
        let config = parse_config("notify_on_change = true", Path::new("config.toml")).unwrap();
        assert!(config.notify_on_change);
    }

    #[test]
    fn clipboard_change_identifies_changed_selections() {
        let original = (Some("primary".into()), Some("regular".into()));
        assert_eq!(clipboard_change(&original, &original), None);
        assert_eq!(
            clipboard_change(&(None, original.1.clone()), &original),
            Some(ClipboardChange::Primary)
        );
        assert_eq!(
            clipboard_change(&(original.0.clone(), None), &original),
            Some(ClipboardChange::Regular)
        );
        assert_eq!(
            clipboard_change(&(None, None), &original),
            Some(ClipboardChange::Both)
        );
    }

    #[test]
    fn config_parses_icon_name() {
        let config = parse_config("icon_name = \"edit-copy\"", Path::new("config.toml")).unwrap();
        assert_eq!(config.icon_name, "edit-copy");
    }

    #[test]
    fn config_rejects_empty_icon_name() {
        let error = parse_config("icon_name = \"  \"", Path::new("config.toml")).unwrap_err();
        assert!(error.contains("icon_name must not be empty"));
    }

    #[test]
    fn config_validates_stack_size() {
        let config = parse_config("stack_size = 4", Path::new("config.toml")).unwrap();
        assert_eq!(config.stack_size, 4);

        for invalid in [0, 17] {
            let error = parse_config(&format!("stack_size = {invalid}"), Path::new("config.toml"))
                .unwrap_err();
            assert!(error.contains("stack_size must be between 1 and 16"));
        }
    }

    #[test]
    fn config_parses_click_actions() {
        let config = parse_config(
            "left_click = \"RESET\"\nmiddle_click = \"COPY_PRIMARY\"",
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(config.left_click, ClipboardAction::Reset);
        assert_eq!(config.middle_click, ClipboardAction::CopyPrimary);
    }

    #[test]
    fn config_rejects_zero_polling_period() {
        let error = parse_config("polling_period_ms = 0", Path::new("config.toml")).unwrap_err();
        assert!(error.contains("must be greater than zero"));
    }

    #[test]
    fn preview_leaves_short_text_unchanged() {
        assert_eq!(preview(Some("hello"), false), "hello");
    }

    #[test]
    fn preview_counts_characters_not_bytes() {
        let text = format!("{}éé", "x".repeat(PREVIEW_CHARS));
        assert_eq!(
            preview(Some(&text), false),
            format!("{}... (and 2 more chars)", "x".repeat(PREVIEW_CHARS))
        );
    }

    #[test]
    fn preview_keeps_tooltip_on_one_line() {
        assert_eq!(preview(Some("one\ntwo\tthree"), false), "one two three");
    }

    #[test]
    fn hidden_preview_only_shows_character_count() {
        assert_eq!(preview(Some("secreté"), true), "7 chars");
        assert_eq!(preview(None, true), "0 chars");
    }

    #[test]
    fn tooltip_has_both_selections() {
        assert_eq!(
            tooltip_text(Some("selected"), Some("copied"), false),
            "primary: selected\nregular: copied"
        );
    }

    #[test]
    fn hidden_tooltip_has_both_lengths() {
        assert_eq!(
            tooltip_text(Some("selected"), Some("é"), true),
            "primary: 8 chars\nregular: 1 char"
        );
    }

    #[test]
    fn clicks_request_the_configured_actions() {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::Reset,
            middle_click: ClipboardAction::CopyPrimary,
            primary: None,
            regular: None,
            stack: Vec::new(),
            hide_content: false,
            notifications_enabled: false,
        };

        ksni::Tray::activate(&mut tray, 0, 0);
        ksni::Tray::secondary_activate(&mut tray, 0, 0);

        assert_eq!(
            event_receiver.try_recv(),
            Ok(AppEvent::Action(ActionRequest {
                action: ClipboardAction::Reset,
                source: ActionSource::LeftClick,
            }))
        );
        assert_eq!(
            event_receiver.try_recv(),
            Ok(AppEvent::Action(ActionRequest {
                action: ClipboardAction::CopyPrimary,
                source: ActionSource::MiddleClick,
            }))
        );
    }

    #[test]
    fn every_action_has_a_notification_message() {
        assert_eq!(
            action_notification(ClipboardAction::CopyRegular),
            "Regular clipboard copied to primary"
        );
        assert_eq!(
            action_notification(ClipboardAction::CopyPrimary),
            "Primary clipboard copied to regular"
        );
        assert_eq!(
            action_notification(ClipboardAction::Reset),
            "Primary and regular clipboards cleared"
        );
        assert_eq!(
            action_notification(ClipboardAction::Switch),
            "Primary and regular clipboards switched"
        );
    }

    #[test]
    fn menu_label_shows_content_and_escapes_mnemonics() {
        assert_eq!(
            menu_label("Primary", Some("secret_value"), false),
            "Primary: secret__value"
        );
    }

    #[test]
    fn menu_label_honors_hidden_content() {
        assert_eq!(
            menu_label("Regular", Some("secret"), true),
            "Regular: 6 chars"
        );
    }

    #[test]
    fn menu_entries_request_all_actions() {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: Some("primary".into()),
            regular: Some("regular".into()),
            stack: Vec::new(),
            hide_content: false,
            notifications_enabled: false,
        };

        for (index, expected) in [
            (1, ClipboardAction::CopyPrimary),
            (6, ClipboardAction::CopyRegular),
            (10, ClipboardAction::Switch),
            (11, ClipboardAction::Reset),
        ] {
            let ksni::MenuItem::Standard(item) = ksni::Tray::menu(&tray).remove(index) else {
                panic!("expected an action menu item");
            };
            assert!(item.enabled);
            (item.activate)(&mut tray);
            assert_eq!(
                event_receiver.try_recv(),
                Ok(AppEvent::Action(ActionRequest {
                    action: expected,
                    source: ActionSource::Menu,
                }))
            );
        }
    }

    #[test]
    fn copy_menu_entries_are_disabled_for_empty_sources() {
        let (event_sender, _) = mpsc::unbounded_channel();
        let tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: None,
            regular: Some(String::new()),
            stack: Vec::new(),
            hide_content: false,
            notifications_enabled: false,
        };
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(menu.len(), 17);

        let ksni::MenuItem::Standard(copy_primary) = &menu[1] else {
            panic!("expected copy-primary menu item");
        };
        let ksni::MenuItem::Standard(copy_regular) = &menu[6] else {
            panic!("expected copy-regular menu item");
        };
        assert!(!copy_primary.enabled);
        assert!(!copy_regular.enabled);
        for index in [2, 3, 7, 8, 10, 11] {
            let ksni::MenuItem::Standard(stack_action) = &menu[index] else {
                panic!("expected disabled clipboard action");
            };
            assert!(!stack_action.enabled);
        }
        let ksni::MenuItem::Standard(empty_stack) = &menu[13] else {
            panic!("expected empty-stack message");
        };
        assert_eq!(empty_stack.label, "No stacked entries yet");
        assert!(!empty_stack.enabled);
    }

    #[test]
    fn menu_contains_separators_and_exit() {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: Some("primary".into()),
            regular: Some("regular".into()),
            stack: Vec::new(),
            hide_content: false,
            notifications_enabled: false,
        };
        let mut menu = ksni::Tray::menu(&tray);

        for index in [4, 9, 12, 14] {
            let ksni::MenuItem::Standard(separator) = &menu[index] else {
                panic!("expected visible separator item");
            };
            assert_eq!(separator.label, "────────────────────");
            assert!(!separator.enabled);
        }

        let ksni::MenuItem::Standard(toggle) = menu.remove(15) else {
            panic!("expected notification toggle menu item");
        };
        assert_eq!(toggle.label, "Enable notifications");
        (toggle.activate)(&mut tray);
        assert_eq!(event_receiver.try_recv(), Ok(AppEvent::ToggleNotifications));

        tray.notifications_enabled = true;
        let mut menu = ksni::Tray::menu(&tray);
        let ksni::MenuItem::Standard(toggle) = &menu[15] else {
            panic!("expected notification toggle menu item");
        };
        assert_eq!(toggle.label, "Disable notifications");

        let ksni::MenuItem::Standard(exit) = menu.remove(16) else {
            panic!("expected exit menu item");
        };
        (exit.activate)(&mut tray);
        assert_eq!(event_receiver.try_recv(), Ok(AppEvent::Exit));
    }

    #[test]
    fn stack_capacity_evicts_the_oldest_entry() {
        let mut stack = vec!["oldest".into(), "middle".into()];
        push_stack(&mut stack, "newest".into(), 2);
        assert_eq!(stack, ["middle", "newest"]);
    }

    #[test]
    fn stack_menu_entries_are_newest_first_and_hidden_when_requested() {
        let (event_sender, _) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: None,
            regular: None,
            stack: vec!["oldest".into(), "newest".into()],
            hide_content: false,
            notifications_enabled: false,
        };

        let menu = ksni::Tray::menu(&tray);
        let ksni::MenuItem::Standard(first) = &menu[13] else {
            panic!("expected first stacked entry");
        };
        let ksni::MenuItem::Standard(second) = &menu[14] else {
            panic!("expected second stacked entry");
        };
        assert_eq!(first.label, "1: newest");
        assert_eq!(second.label, "2: oldest");
        assert!(!first.enabled && !second.enabled);

        tray.hide_content = true;
        assert_eq!(ksni::Tray::menu(&tray).len(), 19);
    }

    #[test]
    fn stack_menu_actions_send_events() {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: Some("primary".into()),
            regular: Some("regular".into()),
            stack: vec!["stacked".into()],
            hide_content: true,
            notifications_enabled: false,
        };

        for (index, expected) in [
            (2, StackAction::PushPrimary),
            (7, StackAction::PushRegular),
            (13, StackAction::PopPrimary),
            (14, StackAction::PopRegular),
            (15, StackAction::PopBoth),
        ] {
            let ksni::MenuItem::Standard(item) = ksni::Tray::menu(&tray).remove(index) else {
                panic!("expected stack action menu item");
            };
            assert!(item.enabled);
            (item.activate)(&mut tray);
            assert_eq!(event_receiver.try_recv(), Ok(AppEvent::Stack(expected)));
        }
    }

    #[test]
    fn clear_menu_actions_send_events() {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut tray = ClipboardTray {
            tooltip: String::new(),
            icon_name: "edit-paste".into(),
            event_sender,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
            primary: Some("primary".into()),
            regular: Some("regular".into()),
            stack: Vec::new(),
            hide_content: false,
            notifications_enabled: false,
        };

        for (index, expected) in [(3, ClearTarget::Primary), (8, ClearTarget::Regular)] {
            let ksni::MenuItem::Standard(item) = ksni::Tray::menu(&tray).remove(index) else {
                panic!("expected clear action menu item");
            };
            assert!(item.enabled);
            (item.activate)(&mut tray);
            assert_eq!(event_receiver.try_recv(), Ok(AppEvent::Clear(expected)));
        }
    }
}

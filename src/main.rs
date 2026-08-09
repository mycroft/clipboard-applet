use std::ffi::OsString;
use std::io::Read;
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

const PREVIEW_CHARS: usize = 40;
const DEFAULT_POLLING_PERIOD_MS: u64 = 1_000;

#[derive(Debug, Eq, PartialEq)]
enum CliAction {
    Run {
        config_file: Option<PathBuf>,
        debug: bool,
        with_notifications: bool,
    },
    Help,
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
    polling_period_ms: u64,
    hide_content: bool,
    notifications: bool,
    icon_name: String,
    stack_size: usize,
    left_click: ClipboardAction,
    middle_click: ClipboardAction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            polling_period_ms: DEFAULT_POLLING_PERIOD_MS,
            hide_content: false,
            notifications: false,
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
    let config = load_config(config_file.as_deref()).unwrap_or_else(|error| {
        eprintln!("failed to load configuration: {error}");
        std::process::exit(1);
    });
    let polling_period = Duration::from_millis(config.polling_period_ms);
    let mut notifications_enabled = config.notifications || with_notifications;
    if debug {
        let config_name = config_file
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "XDG default".into());
        eprintln!(
            "[debug] started: config={}, polling_period_ms={}, icon_name={}, stack_size={}, left_click={}, middle_click={}, notifications={}",
            config_name,
            config.polling_period_ms,
            config.icon_name,
            config.stack_size,
            config.left_click.name(),
            config.middle_click.name(),
            notifications_enabled
        );
    }
    let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
    let mut stack = Vec::new();

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

    let mut poll_interval = tokio::time::interval(polling_period);
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = poll_interval.tick() => {}
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

        let (primary, regular) = tokio::task::spawn_blocking(read_clipboards)
            .await
            .unwrap_or_else(|error| {
                eprintln!("clipboard reader stopped unexpectedly: {error}");
                (None, None)
            });
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

fn print_help() {
    println!(
        "{name} {version}\n\nWayland clipboard tray applet\n\nUsage: {name} [OPTIONS]\n\nOptions:\n  -c, --config-file <PATH>  Use this configuration file\n  -d, --debug               Log clipboard actions to stderr\n      --with-notifications  Enable desktop notifications\n  -h, --help                Show this help",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION")
    );
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<CliAction, String> {
    let mut args = args.into_iter();
    let mut config_file = None;
    let mut debug = false;
    let mut with_notifications = false;
    while let Some(argument) = args.next() {
        let value = match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-d" | "--debug") => {
                debug = true;
                continue;
            }
            Some("--with-notifications") => {
                with_notifications = true;
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
    (
        read_clipboard(ClipboardType::Primary),
        read_clipboard(ClipboardType::Regular),
    )
}

fn read_clipboard(clipboard: ClipboardType) -> Option<String> {
    match try_read_clipboard(clipboard) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("could not read clipboard: {error}");
            None
        }
    }
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

    #[test]
    fn config_file_short_option_is_parsed() {
        assert_eq!(
            parse_args([OsString::from("-c"), OsString::from("custom.toml")]),
            Ok(CliAction::Run {
                config_file: Some(PathBuf::from("custom.toml")),
                debug: false,
                with_notifications: false,
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
                with_notifications: false,
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
                with_notifications: false,
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
                with_notifications: false,
            })
        );
        assert_eq!(
            parse_args([OsString::from("--debug")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: true,
                with_notifications: false,
            })
        );
    }

    #[test]
    fn notifications_option_is_parsed() {
        assert_eq!(
            parse_args([OsString::from("--with-notifications")]),
            Ok(CliAction::Run {
                config_file: None,
                debug: false,
                with_notifications: true,
            })
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
        assert!(!config.hide_content);
        assert!(!config.notifications);
        assert_eq!(config.icon_name, "edit-paste");
        assert_eq!(config.stack_size, 16);
        assert_eq!(config.left_click, ClipboardAction::CopyPrimary);
        assert_eq!(config.middle_click, ClipboardAction::Switch);
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

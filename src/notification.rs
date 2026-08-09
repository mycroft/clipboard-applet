use crate::config::Config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotificationMode {
    Disabled,
    Enabled,
    All,
}

pub(crate) fn settings(config: &Config, override_mode: Option<NotificationMode>) -> (bool, bool) {
    match override_mode {
        None => (config.notifications, config.notify_on_change),
        Some(NotificationMode::Disabled) => (false, false),
        Some(NotificationMode::Enabled) => (true, false),
        Some(NotificationMode::All) => (true, true),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardChange {
    Primary,
    Regular,
    Both,
}

pub(crate) fn clipboard_change(
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

pub(crate) fn send(body: &str, context: &str) -> bool {
    if let Err(error) = notify_rust::Notification::new()
        .summary("Clipboard applet")
        .body(body)
        .icon("edit-paste")
        .show()
    {
        eprintln!("could not send {context} notification: {error}");
        false
    } else {
        true
    }
}

pub(crate) fn send_if_enabled(body: &str, context: &str, enabled: bool) -> bool {
    if enabled { send(body, context) } else { false }
}

pub(crate) fn send_change(change: ClipboardChange, debug: bool) {
    let body = match change {
        ClipboardChange::Primary => "Primary clipboard changed",
        ClipboardChange::Regular => "Regular clipboard changed",
        ClipboardChange::Both => "Primary and regular clipboards changed",
    };
    if send(body, "clipboard change") && debug {
        eprintln!("[debug] clipboard change notification sent: {change:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_clipboard_change() {
        let empty = (None, None);
        assert_eq!(
            clipboard_change(&empty, &(Some("p".into()), None)),
            Some(ClipboardChange::Primary)
        );
        assert_eq!(
            clipboard_change(&empty, &(None, Some("r".into()))),
            Some(ClipboardChange::Regular)
        );
        assert_eq!(
            clipboard_change(&empty, &(Some("p".into()), Some("r".into()))),
            Some(ClipboardChange::Both)
        );
        assert_eq!(clipboard_change(&empty, &empty), None);
    }

    #[test]
    fn command_line_mode_overrides_configuration() {
        let config = Config {
            notifications: true,
            notify_on_change: true,
            ..Config::default()
        };
        assert_eq!(settings(&config, None), (true, true));
        assert_eq!(
            settings(&config, Some(NotificationMode::Disabled)),
            (false, false)
        );
        assert_eq!(
            settings(&config, Some(NotificationMode::Enabled)),
            (true, false)
        );
        assert_eq!(settings(&config, Some(NotificationMode::All)), (true, true));
    }
}

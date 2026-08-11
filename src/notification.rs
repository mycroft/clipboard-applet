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

pub(crate) type ClipboardObservations = (Option<Option<String>>, Option<Option<String>>);

pub(crate) fn clipboard_change(
    previous: &mut ClipboardObservations,
    current: &ClipboardObservations,
) -> Option<ClipboardChange> {
    let primary_changed = observed_value_changed(&previous.0, &current.0);
    let regular_changed = observed_value_changed(&previous.1, &current.1);
    if current.0.is_some() {
        previous.0 = current.0.clone();
    }
    if current.1.is_some() {
        previous.1 = current.1.clone();
    }
    match (primary_changed, regular_changed) {
        (true, true) => Some(ClipboardChange::Both),
        (true, false) => Some(ClipboardChange::Primary),
        (false, true) => Some(ClipboardChange::Regular),
        (false, false) => None,
    }
}

fn observed_value_changed(
    previous: &Option<Option<String>>,
    current: &Option<Option<String>>,
) -> bool {
    matches!((previous, current), (Some(previous), Some(current)) if previous != current)
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
        let mut previous = (Some(None), Some(None));
        assert_eq!(
            clipboard_change(&mut previous, &(Some(Some("p".into())), Some(None))),
            Some(ClipboardChange::Primary)
        );
        previous = (Some(None), Some(None));
        assert_eq!(
            clipboard_change(&mut previous, &(Some(None), Some(Some("r".into())))),
            Some(ClipboardChange::Regular)
        );
        previous = (Some(None), Some(None));
        assert_eq!(
            clipboard_change(
                &mut previous,
                &(Some(Some("p".into())), Some(Some("r".into())))
            ),
            Some(ClipboardChange::Both)
        );
        assert_eq!(
            clipboard_change(
                &mut previous,
                &(Some(Some("p".into())), Some(Some("r".into())))
            ),
            None
        );
    }

    #[test]
    fn unavailable_selection_does_not_hide_healthy_changes_or_advance_history() {
        let mut previous = (
            Some(Some("old primary".into())),
            Some(Some("old regular".into())),
        );
        assert_eq!(
            clipboard_change(&mut previous, &(None, Some(Some("new regular".into())))),
            Some(ClipboardChange::Regular)
        );
        assert_eq!(previous.0, Some(Some("old primary".into())));
        assert_eq!(
            clipboard_change(&mut previous, &(Some(Some("old primary".into())), None)),
            None
        );
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

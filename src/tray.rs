use ksni::ToolTip;
use tokio::sync::mpsc;

use crate::clipboard::{ActionRequest, ActionSource, ClearTarget, ClipboardAction, read_both};
use crate::stack::StackAction;

const PREVIEW_CHARS: usize = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    Action(ActionRequest),
    Stack(StackAction),
    Clear(ClearTarget),
    ToggleNotifications,
    Exit,
}

#[derive(Debug)]
pub(crate) struct ClipboardTray {
    pub(crate) tooltip: String,
    pub(crate) icon_name: String,
    pub(crate) event_sender: mpsc::UnboundedSender<AppEvent>,
    pub(crate) left_click: ClipboardAction,
    pub(crate) middle_click: ClipboardAction,
    pub(crate) primary: Option<String>,
    pub(crate) regular: Option<String>,
    pub(crate) stack: Vec<String>,
    pub(crate) hide_content: bool,
    pub(crate) notifications_enabled: bool,
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
        let stack_item = |label: &str, action: StackAction, enabled: bool| {
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
        let any_content =
            has_content(self.primary.as_deref()) || has_content(self.regular.as_deref());
        let mut menu = vec![
            preview_item("Primary", self.primary.as_deref()),
            action_item(
                "Copy primary to regular",
                ClipboardAction::CopyPrimary,
                has_content(self.primary.as_deref()),
            ),
            stack_item(
                "Stack primary",
                StackAction::PushPrimary,
                has_content(self.primary.as_deref()),
            ),
            clear_item(
                "Clear primary",
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
            stack_item(
                "Stack regular",
                StackAction::PushRegular,
                has_content(self.regular.as_deref()),
            ),
            clear_item(
                "Clear regular",
                ClearTarget::Regular,
                has_content(self.regular.as_deref()),
            ),
            separator_item(),
            action_item("Switch clipboards", ClipboardAction::Switch, any_content),
            action_item("Reset clipboards", ClipboardAction::Reset, any_content),
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
                stack_item("Pop to primary", StackAction::PopPrimary, true),
                stack_item("Pop to regular", StackAction::PopRegular, true),
                stack_item("Pop to primary and regular", StackAction::PopBoth, true),
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
        (self.primary, self.regular) = read_both();
    }
}

pub(crate) fn tooltip_text(primary: Option<&str>, regular: Option<&str>, hide: bool) -> String {
    format!(
        "primary: {}\nregular: {}",
        preview(primary, hide),
        preview(regular, hide)
    )
}

fn menu_label(name: &str, value: Option<&str>, hide: bool) -> String {
    format!("{name}: {}", preview(value, hide)).replace('_', "__")
}

fn has_content(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn preview(value: Option<&str>, hide: bool) -> String {
    if hide {
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
    fn preview_hides_content_and_counts_characters() {
        assert_eq!(preview(Some("é🙂"), true), "2 chars");
        assert_eq!(preview(None, true), "0 chars");
    }

    #[test]
    fn preview_flattens_control_whitespace() {
        assert_eq!(preview(Some("one\ntwo\tthree"), false), "one two three");
    }

    #[test]
    fn preview_truncates_by_characters() {
        let value = "🙂".repeat(PREVIEW_CHARS + 3);
        assert_eq!(
            preview(Some(&value), false),
            format!("{}... (and 3 more chars)", "🙂".repeat(PREVIEW_CHARS))
        );
    }

    #[test]
    fn menu_labels_escape_underscores() {
        assert_eq!(
            menu_label("Primary", Some("one_two"), false),
            "Primary: one__two"
        );
    }
}

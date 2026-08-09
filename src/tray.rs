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
    Edit(EditTarget),
    ToggleNotifications,
    Exit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditTarget {
    Primary,
    Regular,
    Stack(usize),
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
    pub(crate) editor_enabled: bool,
    pub(crate) stack_enabled: bool,
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
        let edit_item = |label: String, target: EditTarget, enabled: bool| {
            StandardItem {
                label,
                enabled,
                activate: Box::new(move |tray: &mut Self| {
                    let _ = tray.event_sender.send(AppEvent::Edit(target));
                }),
                ..Default::default()
            }
            .into()
        };
        let any_content =
            has_content(self.primary.as_deref()) || has_content(self.regular.as_deref());
        let mut menu = vec![preview_item("Primary", self.primary.as_deref())];
        if self.editor_enabled {
            menu.push(edit_item(
                "Edit primary".into(),
                EditTarget::Primary,
                has_content(self.primary.as_deref()),
            ));
        }
        menu.push(action_item(
            "Copy primary to regular",
            ClipboardAction::CopyPrimary,
            has_content(self.primary.as_deref()),
        ));
        if self.stack_enabled {
            menu.push(stack_item(
                "Stack primary",
                StackAction::PushPrimary,
                has_content(self.primary.as_deref()),
            ));
        }
        menu.extend([
            clear_item(
                "Clear primary",
                ClearTarget::Primary,
                has_content(self.primary.as_deref()),
            ),
            separator_item(),
            preview_item("Regular", self.regular.as_deref()),
        ]);
        if self.editor_enabled {
            menu.push(edit_item(
                "Edit regular".into(),
                EditTarget::Regular,
                has_content(self.regular.as_deref()),
            ));
        }
        menu.push(action_item(
            "Copy regular to primary",
            ClipboardAction::CopyRegular,
            has_content(self.regular.as_deref()),
        ));
        if self.stack_enabled {
            menu.push(stack_item(
                "Stack regular",
                StackAction::PushRegular,
                has_content(self.regular.as_deref()),
            ));
        }
        menu.extend([
            clear_item(
                "Clear regular",
                ClearTarget::Regular,
                has_content(self.regular.as_deref()),
            ),
            separator_item(),
            action_item("Switch clipboards", ClipboardAction::Switch, any_content),
            action_item("Reset clipboards", ClipboardAction::Reset, any_content),
        ]);
        if self.stack_enabled {
            menu.push(separator_item());
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
                if self.editor_enabled {
                    menu.extend(self.stack.iter().rev().enumerate().map(
                        |(display_index, value)| {
                            let stack_index = self.stack.len() - 1 - display_index;
                            let label = if self.hide_content {
                                format!("Edit stacked entry {}", display_index + 1)
                            } else {
                                format!(
                                    "Edit {}: {}",
                                    display_index + 1,
                                    preview(Some(value), false)
                                )
                                .replace('_', "__")
                            };
                            edit_item(label, EditTarget::Stack(stack_index), true)
                        },
                    ));
                } else if !self.hide_content {
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

    fn editable_tray() -> (ClipboardTray, mpsc::UnboundedReceiver<AppEvent>) {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        (
            ClipboardTray {
                tooltip: String::new(),
                icon_name: "edit-paste".into(),
                event_sender,
                left_click: ClipboardAction::CopyPrimary,
                middle_click: ClipboardAction::Switch,
                primary: Some("primary".into()),
                regular: Some("regular".into()),
                stack: vec!["oldest".into(), "newest".into()],
                hide_content: false,
                notifications_enabled: false,
                editor_enabled: true,
                stack_enabled: true,
            },
            event_receiver,
        )
    }

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

    #[test]
    fn edit_menu_items_send_selection_and_stack_targets() {
        let (mut tray, mut receiver) = editable_tray();
        for (label, expected) in [
            ("Edit primary", EditTarget::Primary),
            ("Edit regular", EditTarget::Regular),
            ("Edit 1: newest", EditTarget::Stack(1)),
            ("Edit 2: oldest", EditTarget::Stack(0)),
        ] {
            let item = ksni::Tray::menu(&tray)
                .into_iter()
                .find_map(|item| match item {
                    ksni::MenuItem::Standard(item) if item.label == label => Some(item),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing menu item {label}"));
            assert!(item.enabled);
            (item.activate)(&mut tray);
            assert_eq!(receiver.try_recv(), Ok(AppEvent::Edit(expected)));
        }
    }

    #[test]
    fn edit_menu_items_are_hidden_without_an_editor() {
        let (mut tray, _) = editable_tray();
        tray.editor_enabled = false;
        let labels: Vec<String> = ksni::Tray::menu(&tray)
            .into_iter()
            .filter_map(|item| match item {
                ksni::MenuItem::Standard(item) => Some(item.label),
                _ => None,
            })
            .collect();
        assert!(labels.iter().all(|label| !label.starts_with("Edit")));
        assert!(labels.iter().any(|label| label == "1: newest"));
        assert!(labels.iter().any(|label| label == "2: oldest"));
    }

    #[test]
    fn stack_menu_items_are_hidden_when_stack_is_disabled() {
        let (mut tray, _) = editable_tray();
        tray.stack_enabled = false;
        let labels: Vec<String> = ksni::Tray::menu(&tray)
            .into_iter()
            .filter_map(|item| match item {
                ksni::MenuItem::Standard(item) => Some(item.label),
                _ => None,
            })
            .collect();
        assert!(labels.iter().all(|label| !label.starts_with("Stack ")));
        assert!(labels.iter().all(|label| !label.starts_with("Pop to ")));
        assert!(labels.iter().all(|label| !label.starts_with("Edit 1:")));
        assert!(!labels.iter().any(|label| label == "No stacked entries yet"));
    }
}

use ksni::ToolTip;
use tokio::sync::mpsc;

use crate::clipboard::{
    ActionRequest, ActionSource, ClearTarget, ClipboardAction, ClipboardRead, read_both,
};
use crate::stack::{StackAction, StackEntry};

const PREVIEW_CHARS: usize = 40;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    Action(ActionRequest),
    Stack(StackAction),
    Clear(ClearTarget),
    Edit(EditTarget),
    CancelEdit,
    ToggleNotifications,
    Exit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditTarget {
    Primary,
    Regular,
    Stack(u64),
}

#[derive(Debug)]
pub(crate) struct ClipboardTray {
    pub(crate) tooltip: String,
    pub(crate) icon_name: String,
    pub(crate) event_sender: mpsc::UnboundedSender<AppEvent>,
    pub(crate) left_click: ClipboardAction,
    pub(crate) middle_click: ClipboardAction,
    pub(crate) primary: ClipboardRead,
    pub(crate) regular: ClipboardRead,
    pub(crate) stack: Vec<StackEntry>,
    pub(crate) hide_content: bool,
    pub(crate) notifications_enabled: bool,
    pub(crate) editor_enabled: bool,
    pub(crate) editor_target: Option<EditTarget>,
    pub(crate) stack_enabled: bool,
    pub(crate) max_clipboard_bytes: u64,
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
        let preview_item = |name: &str, value: &ClipboardRead| {
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
        let any_content = has_content(&self.primary) || has_content(&self.regular);
        let switch_enabled =
            any_content && is_switchable(&self.primary) && is_switchable(&self.regular);
        let mut menu = Vec::new();
        if let Some(target) = self.editor_target {
            menu.push(
                StandardItem {
                    label: editing_label(target, &self.stack),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            menu.push(
                StandardItem {
                    label: "Cancel edit".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray.event_sender.send(AppEvent::CancelEdit);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            menu.push(separator_item());
        }
        menu.push(preview_item("Primary", &self.primary));
        if self.editor_enabled {
            menu.push(edit_item(
                "Edit primary".into(),
                EditTarget::Primary,
                self.editor_target.is_none() && has_content(&self.primary),
            ));
        }
        menu.push(action_item(
            "Copy primary to regular",
            ClipboardAction::CopyPrimary,
            has_content(&self.primary),
        ));
        if self.stack_enabled {
            menu.push(stack_item(
                "Stack primary",
                StackAction::PushPrimary,
                has_content(&self.primary),
            ));
        }
        menu.extend([
            clear_item(
                "Clear primary",
                ClearTarget::Primary,
                has_content(&self.primary),
            ),
            separator_item(),
            preview_item("Regular", &self.regular),
        ]);
        if self.editor_enabled {
            menu.push(edit_item(
                "Edit regular".into(),
                EditTarget::Regular,
                self.editor_target.is_none() && has_content(&self.regular),
            ));
        }
        menu.push(action_item(
            "Copy regular to primary",
            ClipboardAction::CopyRegular,
            has_content(&self.regular),
        ));
        if self.stack_enabled {
            menu.push(stack_item(
                "Stack regular",
                StackAction::PushRegular,
                has_content(&self.regular),
            ));
        }
        menu.extend([
            clear_item(
                "Clear regular",
                ClearTarget::Regular,
                has_content(&self.regular),
            ),
            separator_item(),
            action_item("Switch clipboards", ClipboardAction::Switch, switch_enabled),
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
                        |(display_index, entry)| {
                            let label = if self.hide_content {
                                format!("Edit stacked entry {}", display_index + 1)
                            } else {
                                format!(
                                    "Edit {}: {}",
                                    display_index + 1,
                                    preview_text(&entry.value, false)
                                )
                                .replace('_', "__")
                            };
                            edit_item(
                                label,
                                EditTarget::Stack(entry.id),
                                self.editor_target.is_none(),
                            )
                        },
                    ));
                } else if !self.hide_content {
                    menu.extend(self.stack.iter().rev().enumerate().map(|(index, entry)| {
                        StandardItem {
                            label: format!("{}: {}", index + 1, preview_text(&entry.value, false))
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
        (self.primary, self.regular) = read_both(self.max_clipboard_bytes);
    }
}

pub(crate) fn tooltip_text(primary: &ClipboardRead, regular: &ClipboardRead, hide: bool) -> String {
    format!(
        "primary: {}\nregular: {}",
        preview(primary, hide),
        preview(regular, hide)
    )
}

fn menu_label(name: &str, value: &ClipboardRead, hide: bool) -> String {
    format!("{name}: {}", preview(value, hide)).replace('_', "__")
}

fn has_content(value: &ClipboardRead) -> bool {
    matches!(value, ClipboardRead::Text(value) if !value.is_empty())
}

fn is_switchable(value: &ClipboardRead) -> bool {
    matches!(value, ClipboardRead::Text(_) | ClipboardRead::Empty)
}

fn editing_label(target: EditTarget, stack: &[StackEntry]) -> String {
    match target {
        EditTarget::Primary => "Editing primary".into(),
        EditTarget::Regular => "Editing regular".into(),
        EditTarget::Stack(id) => stack
            .iter()
            .position(|entry| entry.id == id)
            .map(|index| format!("Editing stacked entry {}", stack.len() - index))
            .unwrap_or_else(|| "Editing removed stacked entry".into()),
    }
}

fn preview(value: &ClipboardRead, hide: bool) -> String {
    match value {
        ClipboardRead::Text(value) => preview_text(value, hide),
        ClipboardRead::Empty => if hide { "0 chars" } else { "(empty)" }.into(),
        ClipboardRead::NonText => "(non-text)".into(),
        ClipboardRead::Unsupported => "(unsupported)".into(),
        ClipboardRead::Oversized { limit } => format!("(over {limit} bytes)"),
        ClipboardRead::Error(_) => "(unavailable)".into(),
    }
}

fn preview_text(value: &str, hide: bool) -> String {
    if hide {
        let length = value.chars().count();
        return format!("{length} {}", if length == 1 { "char" } else { "chars" });
    }
    let total = value.chars().count();
    let visible: String = value
        .chars()
        .take(PREVIEW_CHARS)
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            character => character,
        })
        .collect();
    if total <= PREVIEW_CHARS {
        return visible;
    }
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
                primary: ClipboardRead::Text("primary".into()),
                regular: ClipboardRead::Text("regular".into()),
                stack: vec![
                    StackEntry::new("oldest".into()),
                    StackEntry::new("newest".into()),
                ],
                hide_content: false,
                notifications_enabled: false,
                editor_enabled: true,
                editor_target: None,
                stack_enabled: true,
                max_clipboard_bytes: 1024,
            },
            event_receiver,
        )
    }

    #[test]
    fn preview_hides_content_and_counts_characters() {
        assert_eq!(preview(&ClipboardRead::Text("é🙂".into()), true), "2 chars");
        assert_eq!(preview(&ClipboardRead::Empty, true), "0 chars");
    }

    #[test]
    fn preview_flattens_control_whitespace() {
        assert_eq!(preview_text("one\ntwo\tthree", false), "one two three");
    }

    #[test]
    fn preview_truncates_by_characters() {
        let value = "🙂".repeat(PREVIEW_CHARS + 3);
        assert_eq!(
            preview_text(&value, false),
            format!("{}... (and 3 more chars)", "🙂".repeat(PREVIEW_CHARS))
        );
    }

    #[test]
    fn menu_labels_escape_underscores() {
        assert_eq!(
            menu_label("Primary", &ClipboardRead::Text("one_two".into()), false),
            "Primary: one__two"
        );
    }

    #[test]
    fn preview_reports_each_unavailable_state_independently() {
        assert_eq!(preview(&ClipboardRead::NonText, false), "(non-text)");
        assert_eq!(preview(&ClipboardRead::Unsupported, false), "(unsupported)");
        assert_eq!(
            preview(&ClipboardRead::Oversized { limit: 1024 }, false),
            "(over 1024 bytes)"
        );
        assert_eq!(
            preview(&ClipboardRead::Error("failed".into()), false),
            "(unavailable)"
        );
        assert_eq!(
            tooltip_text(
                &ClipboardRead::Error("failed".into()),
                &ClipboardRead::Text("healthy".into()),
                false
            ),
            "primary: (unavailable)\nregular: healthy"
        );
    }

    #[test]
    fn healthy_selection_actions_remain_enabled_when_the_other_read_fails() {
        let (mut tray, _) = editable_tray();
        tray.primary = ClipboardRead::Error("failed".into());
        let items = ksni::Tray::menu(&tray);
        let enabled = |label: &str| {
            items.iter().find_map(|item| match item {
                ksni::MenuItem::Standard(item) if item.label == label => Some(item.enabled),
                _ => None,
            })
        };
        assert_eq!(enabled("Copy primary to regular"), Some(false));
        assert_eq!(enabled("Copy regular to primary"), Some(true));
        assert_eq!(enabled("Switch clipboards"), Some(false));
        assert_eq!(enabled("Reset clipboards"), Some(true));
    }

    #[test]
    fn edit_menu_items_send_selection_and_stack_targets() {
        let (mut tray, mut receiver) = editable_tray();
        let newest_id = tray.stack[1].id;
        let oldest_id = tray.stack[0].id;
        for (label, expected) in [
            ("Edit primary", EditTarget::Primary),
            ("Edit regular", EditTarget::Regular),
            ("Edit 1: newest", EditTarget::Stack(newest_id)),
            ("Edit 2: oldest", EditTarget::Stack(oldest_id)),
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
    fn editor_busy_state_identifies_target_and_can_be_cancelled() {
        let (mut tray, mut receiver) = editable_tray();
        tray.editor_target = Some(EditTarget::Primary);
        let items = ksni::Tray::menu(&tray);
        assert!(items.iter().any(|item| {
            matches!(item, ksni::MenuItem::Standard(item) if item.label == "Editing primary" && !item.enabled)
        }));
        assert!(items.iter().all(|item| {
            !matches!(item, ksni::MenuItem::Standard(item) if item.label.starts_with("Edit ") && item.enabled)
        }));
        let cancel = items
            .into_iter()
            .find_map(|item| match item {
                ksni::MenuItem::Standard(item) if item.label == "Cancel edit" => Some(item),
                _ => None,
            })
            .unwrap();
        (cancel.activate)(&mut tray);
        assert_eq!(receiver.try_recv(), Ok(AppEvent::CancelEdit));
    }

    #[test]
    fn editor_busy_state_identifies_stack_target_without_content() {
        let (mut tray, _) = editable_tray();
        let oldest_id = tray.stack[0].id;
        tray.editor_target = Some(EditTarget::Stack(oldest_id));
        assert!(ksni::Tray::menu(&tray).iter().any(|item| {
            matches!(item, ksni::MenuItem::Standard(item) if item.label == "Editing stacked entry 2")
        }));
        tray.stack.clear();
        assert!(ksni::Tray::menu(&tray).iter().any(|item| {
            matches!(item, ksni::MenuItem::Standard(item) if item.label == "Editing removed stacked entry")
        }));
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

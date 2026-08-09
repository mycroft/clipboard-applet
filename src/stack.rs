use wl_clipboard_rs::copy::ClipboardType as CopyClipboardType;
use wl_clipboard_rs::paste::ClipboardType;

use crate::clipboard::{length, try_read, try_read_both, write, write_both_with_rollback};
use crate::notification;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackAction {
    PushPrimary,
    PushRegular,
    PopPrimary,
    PopRegular,
    PopBoth,
}

pub(crate) fn perform(
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
            let value = try_read(clipboard)?
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "source clipboard is empty".to_string())?;
            if debug {
                eprintln!(
                    "[debug] stack push: source={clipboard:?}, length={} chars",
                    value.chars().count()
                );
            }
            push(stack, value, capacity);
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
            write(clipboard, Some(value), debug)?;
            stack.pop();
        }
        StackAction::PopBoth => pop_to_both(stack, debug, try_read_both, write)?,
    }
    let notification_sent =
        notification::send_if_enabled(notification_body(action), "clipboard stack", notifications);
    if notification_sent && debug {
        eprintln!("[debug] stack notification sent: {action:?}");
    } else if !notifications && debug {
        eprintln!("[debug] stack notification skipped: disabled");
    }
    Ok(())
}

fn pop_to_both<FRead, FWrite>(
    stack: &mut Vec<String>,
    debug: bool,
    read: FRead,
    write: FWrite,
) -> Result<(), String>
where
    FRead: FnOnce() -> Result<(Option<String>, Option<String>), String>,
    FWrite: FnMut(CopyClipboardType, Option<String>, bool) -> Result<(), String>,
{
    let value = stack
        .last()
        .cloned()
        .ok_or_else(|| "clipboard stack is empty".to_string())?;
    let (original_primary, original_regular) = read()?;
    if debug {
        eprintln!(
            "[debug] stack pop: destination=Primary+Regular, length={} chars, original_primary={} chars, original_regular={} chars",
            value.chars().count(),
            length(original_primary.as_deref()),
            length(original_regular.as_deref())
        );
    }
    write_both_with_rollback(
        Some(value.clone()),
        Some(value),
        original_primary,
        debug,
        write,
    )?;
    stack.pop();
    Ok(())
}

fn push(stack: &mut Vec<String>, value: String, capacity: usize) {
    if stack.len() == capacity {
        stack.remove(0);
    }
    stack.push(value);
}

fn notification_body(action: StackAction) -> &'static str {
    match action {
        StackAction::PushPrimary => "Primary clipboard stacked",
        StackAction::PushRegular => "Regular clipboard stacked",
        StackAction::PopPrimary => "Stacked entry popped to primary",
        StackAction::PopRegular => "Stacked entry popped to regular",
        StackAction::PopBoth => "Stacked entry popped to primary and regular",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_evicts_oldest_entry_at_capacity() {
        let mut stack = vec!["one".into(), "two".into()];
        push(&mut stack, "three".into(), 2);
        assert_eq!(stack, ["two", "three"]);
    }

    #[test]
    fn failed_pop_to_both_keeps_entry() {
        let mut stack = vec!["value".into()];
        let result = pop_to_both(
            &mut stack,
            false,
            || Ok((Some("old".into()), None)),
            |clipboard, _, _| {
                if clipboard == CopyClipboardType::Regular {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(stack, ["value"]);
    }
}

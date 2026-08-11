use std::sync::atomic::{AtomicU64, Ordering};

use wl_clipboard_rs::copy::ClipboardType as CopyClipboardType;
use wl_clipboard_rs::paste::ClipboardType;

use crate::clipboard::{
    ClipboardRead, length, read, try_read_both, write, write_both_with_rollback,
};
use crate::notification;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackAction {
    PushPrimary,
    PushRegular,
    PopPrimary,
    PopRegular,
    PopBoth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StackEntry {
    pub(crate) id: u64,
    pub(crate) value: String,
}

impl StackEntry {
    pub(crate) fn new(value: String) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            value,
        }
    }
}

pub(crate) fn perform(
    action: StackAction,
    stack: &mut Vec<StackEntry>,
    capacity: usize,
    max_clipboard_bytes: u64,
    max_entry_bytes: u64,
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
            let source = read(clipboard, max_clipboard_bytes);
            if debug {
                eprintln!(
                    "[debug] stack push: source={clipboard:?}, state={}, length={} chars",
                    source.state(),
                    source.text_length()
                );
            }
            let value = match source {
                ClipboardRead::Text(value) if !value.is_empty() => value,
                ClipboardRead::Text(_) | ClipboardRead::Empty => {
                    return Err("source clipboard is empty".into());
                }
                ClipboardRead::NonText => {
                    return Err("source clipboard does not contain text".into());
                }
                ClipboardRead::Unsupported => return Err("source clipboard is unsupported".into()),
                ClipboardRead::Oversized { limit } => {
                    return Err(format!(
                        "source clipboard exceeds the configured {limit}-byte limit"
                    ));
                }
                ClipboardRead::Error(error) => {
                    return Err(format!("could not read source clipboard: {error}"));
                }
            };
            validate_entry_size(&value, max_entry_bytes)?;
            push(stack, value, capacity);
        }
        StackAction::PopPrimary | StackAction::PopRegular => {
            let value = stack
                .last()
                .map(|entry| entry.value.clone())
                .ok_or_else(|| "clipboard stack is empty".to_string())?;
            validate_clipboard_size(&value, max_clipboard_bytes)?;
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
            write(clipboard, value, debug)?;
            stack.pop();
        }
        StackAction::PopBoth => pop_to_both(
            stack,
            debug,
            max_clipboard_bytes,
            || try_read_both(max_clipboard_bytes),
            write,
        )?,
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

fn validate_entry_size(value: &str, max_bytes: u64) -> Result<(), String> {
    let length = value.len() as u64;
    if length > max_bytes {
        return Err(format!(
            "stack entry is too large ({length} bytes; limit is {max_bytes})"
        ));
    }
    Ok(())
}

fn validate_clipboard_size(value: &str, max_bytes: u64) -> Result<(), String> {
    let length = value.len() as u64;
    if length > max_bytes {
        return Err(format!(
            "stacked value is too large for the clipboard ({length} bytes; limit is {max_bytes})"
        ));
    }
    Ok(())
}

fn pop_to_both<FRead, FWrite>(
    stack: &mut Vec<StackEntry>,
    debug: bool,
    max_clipboard_bytes: u64,
    read: FRead,
    write: FWrite,
) -> Result<(), String>
where
    FRead: FnOnce() -> Result<(Option<String>, Option<String>), String>,
    FWrite: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
{
    let value = stack
        .last()
        .map(|entry| entry.value.clone())
        .ok_or_else(|| "clipboard stack is empty".to_string())?;
    validate_clipboard_size(&value, max_clipboard_bytes)?;
    let (original_primary, original_regular) = read()?;
    let original_primary = original_primary.ok_or_else(|| {
        "cannot safely pop to both when the primary clipboard has no readable text".to_string()
    })?;
    if debug {
        eprintln!(
            "[debug] stack pop: destination=Primary+Regular, length={} chars, original_primary={} chars, original_regular={} chars",
            value.chars().count(),
            original_primary.chars().count(),
            length(original_regular.as_deref())
        );
    }
    write_both_with_rollback(value.clone(), value, original_primary, debug, write)?;
    stack.pop();
    Ok(())
}

fn push(stack: &mut Vec<StackEntry>, value: String, capacity: usize) {
    if stack.len() == capacity {
        stack.remove(0);
    }
    stack.push(StackEntry::new(value));
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

    fn entry(value: &str) -> StackEntry {
        StackEntry::new(value.into())
    }

    fn values(stack: &[StackEntry]) -> Vec<&str> {
        stack.iter().map(|entry| entry.value.as_str()).collect()
    }

    #[test]
    fn push_evicts_oldest_entry_at_capacity() {
        let mut stack = vec![entry("one"), entry("two")];
        push(&mut stack, "three".into(), 2);
        assert_eq!(values(&stack), ["two", "three"]);
        assert_ne!(stack[0].id, stack[1].id);
    }

    #[test]
    fn stack_entry_limit_accepts_below_and_at_but_rejects_above() {
        assert!(validate_entry_size("123", 4).is_ok());
        assert!(validate_entry_size("1234", 4).is_ok());
        assert!(validate_entry_size("12345", 4).is_err());
    }

    #[test]
    fn clipboard_write_limit_accepts_below_and_at_but_rejects_above() {
        assert!(validate_clipboard_size("123", 4).is_ok());
        assert!(validate_clipboard_size("1234", 4).is_ok());
        assert!(validate_clipboard_size("12345", 4).is_err());
    }

    #[test]
    fn failed_pop_to_both_keeps_entry() {
        let mut stack = vec![entry("value")];
        let result = pop_to_both(
            &mut stack,
            false,
            1024,
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
        assert_eq!(values(&stack), ["value"]);
    }

    #[test]
    fn pop_to_both_does_not_write_without_safe_rollback_text() {
        let mut stack = vec![entry("value")];
        let mut writes = Vec::new();
        let result = pop_to_both(
            &mut stack,
            false,
            1024,
            || Ok((None, Some("regular".into()))),
            |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(writes.is_empty());
        assert_eq!(values(&stack), ["value"]);
    }
}

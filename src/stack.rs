use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use wl_clipboard_rs::copy::{ClipboardType as CopyClipboardType, Seat as CopySeat, clear};
use wl_clipboard_rs::paste::ClipboardType;

use crate::clipboard::{
    ClipboardRead, ReadLimits, ServingFailure, read, write, write_both_with_rollback,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StackCopyRequest {
    pub(crate) id: u64,
    pub(crate) destination: StackCopyDestination,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackCopyDestination {
    Primary,
    Regular,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StackEntry {
    pub(crate) id: u64,
    pub(crate) value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StackLimits {
    pub(crate) capacity: usize,
    pub(crate) clipboard: ReadLimits,
    pub(crate) max_entry_bytes: u64,
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
    limits: StackLimits,
    notifications: bool,
    debug: bool,
    failure_sender: mpsc::UnboundedSender<ServingFailure>,
) -> Result<(), String> {
    match action {
        StackAction::PushPrimary | StackAction::PushRegular => {
            let clipboard = if action == StackAction::PushPrimary {
                ClipboardType::Primary
            } else {
                ClipboardType::Regular
            };
            let source = read(clipboard, limits.clipboard);
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
                ClipboardRead::Timeout => {
                    return Err("timed out reading source clipboard".into());
                }
                ClipboardRead::Error(error) => {
                    return Err(format!("could not read source clipboard: {error}"));
                }
            };
            validate_entry_size(&value, limits.max_entry_bytes)?;
            push(stack, value, limits.capacity);
        }
        StackAction::PopPrimary | StackAction::PopRegular => {
            let value = stack
                .last()
                .map(|entry| entry.value.clone())
                .ok_or_else(|| "clipboard stack is empty".to_string())?;
            validate_clipboard_size(&value, limits.clipboard.max_bytes)?;
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
            write(clipboard, value, debug, "STACK_POP", failure_sender.clone())?;
            stack.pop();
        }
        StackAction::PopBoth => pop_to_both(
            stack,
            debug,
            limits.clipboard.max_bytes,
            || read(ClipboardType::Primary, limits.clipboard),
            |clipboard, value, debug| {
                write_optional(
                    clipboard,
                    value,
                    debug,
                    "STACK_POP_BOTH",
                    failure_sender.clone(),
                )
            },
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

pub(crate) fn copy_entry(
    request: StackCopyRequest,
    stack: &[StackEntry],
    max_clipboard_bytes: u64,
    notifications: bool,
    debug: bool,
    failure_sender: mpsc::UnboundedSender<ServingFailure>,
) -> Result<(), String> {
    copy_entry_with(
        request,
        stack,
        max_clipboard_bytes,
        debug,
        |clipboard, value, debug| {
            write(
                clipboard,
                value,
                debug,
                "STACK_COPY",
                failure_sender.clone(),
            )
        },
    )?;
    let body = match request.destination {
        StackCopyDestination::Primary => "Stacked entry copied to primary",
        StackCopyDestination::Regular => "Stacked entry copied to regular",
    };
    let sent = notification::send_if_enabled(body, "clipboard stack", notifications);
    if sent && debug {
        eprintln!(
            "[debug] stack copy notification sent: destination={:?}",
            request.destination
        );
    }
    Ok(())
}

fn copy_entry_with<FWrite>(
    request: StackCopyRequest,
    stack: &[StackEntry],
    max_clipboard_bytes: u64,
    debug: bool,
    mut write: FWrite,
) -> Result<(), String>
where
    FWrite: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
{
    let entry = stack
        .iter()
        .find(|entry| entry.id == request.id)
        .ok_or_else(|| "stacked entry no longer exists".to_string())?;
    validate_clipboard_size(&entry.value, max_clipboard_bytes)?;
    if debug {
        eprintln!(
            "[debug] stack copy: destination={:?}, length={} chars",
            request.destination,
            entry.value.chars().count()
        );
    }
    let destination = match request.destination {
        StackCopyDestination::Primary => CopyClipboardType::Primary,
        StackCopyDestination::Regular => CopyClipboardType::Regular,
    };
    write(destination, entry.value.clone(), debug)
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

fn write_optional(
    clipboard: CopyClipboardType,
    value: Option<String>,
    debug: bool,
    operation: &'static str,
    failure_sender: mpsc::UnboundedSender<ServingFailure>,
) -> Result<(), String> {
    match value {
        Some(value) => write(clipboard, value, debug, operation, failure_sender),
        None => clear(clipboard, CopySeat::All).map_err(|error| error.to_string()),
    }
}

fn pop_to_both<FRead, FWrite>(
    stack: &mut Vec<StackEntry>,
    debug: bool,
    max_clipboard_bytes: u64,
    read: FRead,
    write: FWrite,
) -> Result<(), String>
where
    FRead: FnOnce() -> ClipboardRead,
    FWrite: FnMut(CopyClipboardType, Option<String>, bool) -> Result<(), String>,
{
    let value = stack
        .last()
        .map(|entry| entry.value.clone())
        .ok_or_else(|| "clipboard stack is empty".to_string())?;
    validate_clipboard_size(&value, max_clipboard_bytes)?;
    let original_primary = match read() {
        ClipboardRead::Text(value) => Some(value),
        ClipboardRead::Empty => None,
        ClipboardRead::NonText => {
            return Err("cannot safely pop to both when the primary clipboard is non-text".into());
        }
        ClipboardRead::Unsupported => {
            return Err(
                "cannot safely pop to both when the primary clipboard is unsupported".into(),
            );
        }
        ClipboardRead::Oversized { limit } => {
            return Err(format!(
                "cannot safely pop to both when the primary clipboard exceeds the configured {limit}-byte limit"
            ));
        }
        ClipboardRead::Timeout => {
            return Err("timed out reading primary clipboard".into());
        }
        ClipboardRead::Error(error) => {
            return Err(format!("could not read primary clipboard: {error}"));
        }
    };
    if debug {
        eprintln!(
            "[debug] stack pop: destination=Primary+Regular, length={} chars, original_primary={} chars",
            value.chars().count(),
            original_primary
                .as_deref()
                .map_or(0, |value| value.chars().count())
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
    fn copy_entry_writes_each_destination_without_changing_stack() {
        let stack = vec![entry("oldest"), entry("selected"), entry("newest")];
        let selected_id = stack[1].id;
        for (destination, expected_clipboard) in [
            (StackCopyDestination::Primary, CopyClipboardType::Primary),
            (StackCopyDestination::Regular, CopyClipboardType::Regular),
        ] {
            let mut writes = Vec::new();
            copy_entry_with(
                StackCopyRequest {
                    id: selected_id,
                    destination,
                },
                &stack,
                1024,
                false,
                |clipboard, value, _| {
                    writes.push((clipboard, value));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(writes, [(expected_clipboard, "selected".into())]);
            assert_eq!(values(&stack), ["oldest", "selected", "newest"]);
        }
    }

    #[test]
    fn copy_entry_rejects_stale_and_oversized_targets() {
        let stack = vec![entry("value")];
        let mut writes = Vec::new();
        for (id, limit, expected) in [
            (u64::MAX, 1024, "no longer exists"),
            (stack[0].id, 4, "too large"),
        ] {
            let result = copy_entry_with(
                StackCopyRequest {
                    id,
                    destination: StackCopyDestination::Primary,
                },
                &stack,
                limit,
                false,
                |clipboard, value, _| {
                    writes.push((clipboard, value));
                    Ok(())
                },
            );
            assert!(result.unwrap_err().contains(expected));
        }
        assert!(writes.is_empty());
        assert_eq!(values(&stack), ["value"]);
    }

    #[test]
    fn failed_pop_to_both_keeps_entry() {
        let mut stack = vec![entry("value")];
        let result = pop_to_both(
            &mut stack,
            false,
            1024,
            || ClipboardRead::Text("old".into()),
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
    fn pop_to_both_supports_an_empty_primary() {
        let mut stack = vec![entry("value")];
        let mut writes = Vec::new();
        pop_to_both(
            &mut stack,
            false,
            1024,
            || ClipboardRead::Empty,
            |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            writes,
            [
                (CopyClipboardType::Primary, Some("value".into())),
                (CopyClipboardType::Regular, Some("value".into())),
            ]
        );
        assert!(stack.is_empty());
    }

    #[test]
    fn failed_pop_to_both_restores_an_empty_primary() {
        let mut stack = vec![entry("value")];
        let mut writes = Vec::new();
        let result = pop_to_both(
            &mut stack,
            false,
            1024,
            || ClipboardRead::Empty,
            |clipboard, value, _| {
                writes.push((clipboard, value));
                if writes.len() == 2 {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.unwrap_err().contains("primary clipboard restored"));
        assert_eq!(writes[2], (CopyClipboardType::Primary, None));
        assert_eq!(values(&stack), ["value"]);
    }

    #[test]
    fn pop_to_both_does_not_overwrite_non_text_primary_content() {
        let mut stack = vec![entry("value")];
        let mut writes = Vec::new();
        let result = pop_to_both(
            &mut stack,
            false,
            1024,
            || ClipboardRead::NonText,
            |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("non-text"));
        assert!(writes.is_empty());
        assert_eq!(values(&stack), ["value"]);
    }
}

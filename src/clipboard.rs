use std::io::Read;

use serde::Deserialize;
use wl_clipboard_rs::copy::{
    ClipboardType as CopyClipboardType, MimeType as CopyMimeType, Options, Seat as CopySeat,
    Source, clear,
};
use wl_clipboard_rs::paste::{ClipboardType, Error, MimeType, Seat, get_contents};

use crate::notification;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ClipboardAction {
    CopyRegular,
    CopyPrimary,
    Reset,
    Switch,
}

impl ClipboardAction {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::CopyRegular => "COPY_REGULAR",
            Self::CopyPrimary => "COPY_PRIMARY",
            Self::Reset => "RESET",
            Self::Switch => "SWITCH",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionSource {
    LeftClick,
    MiddleClick,
    Menu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActionRequest {
    pub(crate) action: ClipboardAction,
    pub(crate) source: ActionSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClearTarget {
    Primary,
    Regular,
}

pub(crate) fn read_both() -> (Option<String>, Option<String>) {
    try_read_both().unwrap_or_else(|error| {
        eprintln!("could not read clipboards: {error}");
        (None, None)
    })
}

pub(crate) fn try_read_both() -> Result<(Option<String>, Option<String>), String> {
    Ok((
        try_read(ClipboardType::Primary)?,
        try_read(ClipboardType::Regular)?,
    ))
}

pub(crate) fn try_read(clipboard: ClipboardType) -> Result<Option<String>, String> {
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

pub(crate) fn perform_action(
    action: ClipboardAction,
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    match action {
        ClipboardAction::CopyRegular => {
            let regular = try_read(ClipboardType::Regular)?;
            log_read("COPY_REGULAR", "regular", regular.as_deref(), debug);
            write(CopyClipboardType::Primary, regular, debug)?;
        }
        ClipboardAction::CopyPrimary => {
            let primary = try_read(ClipboardType::Primary)?;
            log_read("COPY_PRIMARY", "primary", primary.as_deref(), debug);
            write(CopyClipboardType::Regular, primary, debug)?;
        }
        ClipboardAction::Reset => {
            if debug {
                eprintln!("[debug] RESET clearing primary and regular clipboards");
            }
            clear(CopyClipboardType::Both, CopySeat::All).map_err(|error| error.to_string())?;
        }
        ClipboardAction::Switch => swap(debug)?,
    }
    let notification_sent =
        notification::send_if_enabled(action_notification(action), "clipboard", notifications);
    if notification_sent && debug {
        eprintln!("[debug] notification sent: {}", action.name());
    } else if !notifications && debug {
        eprintln!("[debug] notification skipped: disabled");
    }
    Ok(())
}

pub(crate) fn perform_clear(
    target: ClearTarget,
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    let (clipboard, body) = match target {
        ClearTarget::Primary => (CopyClipboardType::Primary, "Primary clipboard cleared"),
        ClearTarget::Regular => (CopyClipboardType::Regular, "Regular clipboard cleared"),
    };
    if debug {
        eprintln!("[debug] clearing clipboard from menu: {clipboard:?}");
    }
    clear(clipboard, CopySeat::All).map_err(|error| error.to_string())?;
    let notification_sent = notification::send_if_enabled(body, "clipboard clear", notifications);
    if notification_sent && debug {
        eprintln!("[debug] clear notification sent: {target:?}");
    } else if !notifications && debug {
        eprintln!("[debug] clear notification skipped: disabled");
    }
    Ok(())
}

fn swap(debug: bool) -> Result<(), String> {
    let primary = try_read(ClipboardType::Primary)?;
    let regular = try_read(ClipboardType::Regular)?;
    if debug {
        eprintln!(
            "[debug] SWITCH read: primary={} chars, regular={} chars",
            length(primary.as_deref()),
            length(regular.as_deref())
        );
    }
    write_both_with_rollback(regular, primary.clone(), primary, debug, write)
}

fn log_read(action: &str, name: &str, value: Option<&str>, debug: bool) {
    if debug {
        eprintln!("[debug] {action} read: {name}={} chars", length(value));
    }
}

fn action_notification(action: ClipboardAction) -> &'static str {
    match action {
        ClipboardAction::CopyRegular => "Regular clipboard copied to primary",
        ClipboardAction::CopyPrimary => "Primary clipboard copied to regular",
        ClipboardAction::Reset => "Primary and regular clipboards cleared",
        ClipboardAction::Switch => "Primary and regular clipboards switched",
    }
}

pub(crate) fn length(value: Option<&str>) -> usize {
    value.map_or(0, |value| value.chars().count())
}

pub(crate) fn write_both_with_rollback<F>(
    primary: Option<String>,
    regular: Option<String>,
    original_primary: Option<String>,
    debug: bool,
    mut write: F,
) -> Result<(), String>
where
    F: FnMut(CopyClipboardType, Option<String>, bool) -> Result<(), String>,
{
    if let Err(error) = write(CopyClipboardType::Primary, primary, debug) {
        if debug {
            eprintln!("[debug] multi-write failed: step=Primary, error={error}");
        }
        return Err(format!("could not write primary clipboard: {error}"));
    }
    if let Err(error) = write(CopyClipboardType::Regular, regular, debug) {
        if debug {
            eprintln!("[debug] multi-write failed: step=Regular, error={error}");
        }
        return match write(CopyClipboardType::Primary, original_primary, debug) {
            Ok(()) => {
                if debug {
                    eprintln!("[debug] multi-write rollback completed: destination=Primary");
                }
                Err(format!(
                    "could not write regular clipboard: {error}; primary clipboard restored"
                ))
            }
            Err(rollback_error) => {
                if debug {
                    eprintln!(
                        "[debug] multi-write rollback failed: destination=Primary, error={rollback_error}"
                    );
                }
                Err(format!(
                    "could not write regular clipboard: {error}; could not restore primary clipboard: {rollback_error}"
                ))
            }
        };
    }
    Ok(())
}

pub(crate) fn write(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_unicode_characters() {
        assert_eq!(length(Some("é🙂")), 2);
        assert_eq!(length(None), 0);
    }

    #[test]
    fn pair_write_restores_primary_when_regular_fails() {
        let mut writes = Vec::new();
        let result = write_both_with_rollback(
            Some("new-p".into()),
            Some("new-r".into()),
            Some("old-p".into()),
            false,
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
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[2].1.as_deref(), Some("old-p"));
    }
}

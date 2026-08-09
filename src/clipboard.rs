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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardRead {
    Text(String),
    Empty,
    NonText,
    Unsupported,
    Error(String),
}

impl ClipboardRead {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::Text(_) => "Text",
            Self::Empty => "Empty",
            Self::NonText => "NonText",
            Self::Unsupported => "Unsupported",
            Self::Error(_) => "Error",
        }
    }

    pub(crate) fn text_length(&self) -> usize {
        match self {
            Self::Text(value) => value.chars().count(),
            _ => 0,
        }
    }

    fn into_text(self, source: &str) -> Result<String, String> {
        match self {
            Self::Text(value) => Ok(value),
            Self::Empty => Err(format!("{source} clipboard is empty")),
            Self::NonText => Err(format!("{source} clipboard does not contain text")),
            Self::Unsupported => Err(format!("{source} clipboard is unsupported")),
            Self::Error(error) => Err(format!("could not read {source} clipboard: {error}")),
        }
    }

    fn into_display(self) -> Result<Option<String>, String> {
        match self {
            Self::Text(value) => Ok(Some(value)),
            Self::Empty | Self::NonText | Self::Unsupported => Ok(None),
            Self::Error(error) => Err(error),
        }
    }
}

pub(crate) fn read_both() -> (Option<String>, Option<String>) {
    try_read_both().unwrap_or_else(|error| {
        eprintln!("could not read clipboards: {error}");
        (None, None)
    })
}

pub(crate) fn try_read_both() -> Result<(Option<String>, Option<String>), String> {
    Ok((
        read(ClipboardType::Primary).into_display()?,
        read(ClipboardType::Regular).into_display()?,
    ))
}

pub(crate) fn read(clipboard: ClipboardType) -> ClipboardRead {
    let (mut pipe, _) = match get_contents(clipboard, Seat::Unspecified, MimeType::Text) {
        Ok(contents) => contents,
        Err(Error::ClipboardEmpty) => return ClipboardRead::Empty,
        Err(Error::NoMimeType) => return ClipboardRead::NonText,
        Err(Error::PrimarySelectionUnsupported) => return ClipboardRead::Unsupported,
        Err(error) => return ClipboardRead::Error(error.to_string()),
    };
    let mut bytes = Vec::new();
    match pipe.read_to_end(&mut bytes) {
        Ok(_) => ClipboardRead::Text(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) => ClipboardRead::Error(format!("could not receive contents: {error}")),
    }
}

pub(crate) fn perform_action(
    action: ClipboardAction,
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    apply_action(action, debug, read, write, || {
        clear(CopyClipboardType::Both, CopySeat::All).map_err(|error| error.to_string())
    })?;
    let notification_sent =
        notification::send_if_enabled(action_notification(action), "clipboard", notifications);
    if notification_sent && debug {
        eprintln!("[debug] notification sent: {}", action.name());
    } else if !notifications && debug {
        eprintln!("[debug] notification skipped: disabled");
    }
    Ok(())
}

fn apply_action<FRead, FWrite, FClear>(
    action: ClipboardAction,
    debug: bool,
    mut read: FRead,
    mut write: FWrite,
    mut clear_both: FClear,
) -> Result<(), String>
where
    FRead: FnMut(ClipboardType) -> ClipboardRead,
    FWrite: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
    FClear: FnMut() -> Result<(), String>,
{
    match action {
        ClipboardAction::CopyRegular => {
            copy_read_to(
                "COPY_REGULAR",
                "regular",
                read(ClipboardType::Regular),
                CopyClipboardType::Primary,
                debug,
                &mut write,
            )?;
        }
        ClipboardAction::CopyPrimary => {
            copy_read_to(
                "COPY_PRIMARY",
                "primary",
                read(ClipboardType::Primary),
                CopyClipboardType::Regular,
                debug,
                &mut write,
            )?;
        }
        ClipboardAction::Reset => {
            if debug {
                eprintln!("[debug] RESET clearing primary and regular clipboards");
            }
            clear_both()?;
        }
        ClipboardAction::Switch => switch_reads(
            read(ClipboardType::Primary),
            read(ClipboardType::Regular),
            debug,
            &mut write,
        )?,
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

fn copy_read_to<F>(
    action: &str,
    source_name: &str,
    source: ClipboardRead,
    destination: CopyClipboardType,
    debug: bool,
    mut write: F,
) -> Result<(), String>
where
    F: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
{
    if debug {
        eprintln!(
            "[debug] {action} read: {source_name}=state={}, length={} chars",
            source.state(),
            source.text_length()
        );
    }
    let value = source.into_text(source_name)?;
    write(destination, value, debug)
}

fn switch_reads<F>(
    primary: ClipboardRead,
    regular: ClipboardRead,
    debug: bool,
    write: F,
) -> Result<(), String>
where
    F: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
{
    if debug {
        eprintln!(
            "[debug] SWITCH read: primary=state={}, length={} chars, regular=state={}, length={} chars",
            primary.state(),
            primary.text_length(),
            regular.state(),
            regular.text_length()
        );
    }
    let primary = primary.into_text("primary")?;
    let regular = regular.into_text("regular")?;
    write_both_with_rollback(regular, primary.clone(), primary, debug, write)
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
    primary: String,
    regular: String,
    original_primary: String,
    debug: bool,
    mut write: F,
) -> Result<(), String>
where
    F: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
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
    value: String,
    debug: bool,
) -> Result<(), String> {
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

    fn unreadable_states() -> Vec<ClipboardRead> {
        vec![
            ClipboardRead::Empty,
            ClipboardRead::NonText,
            ClipboardRead::Unsupported,
            ClipboardRead::Error("read failed".into()),
        ]
    }

    #[test]
    fn counts_unicode_characters() {
        assert_eq!(length(Some("é🙂")), 2);
        assert_eq!(length(None), 0);
    }

    #[test]
    fn pair_write_restores_primary_when_regular_fails() {
        let mut writes = Vec::new();
        let result = write_both_with_rollback(
            "new-p".into(),
            "new-r".into(),
            "old-p".into(),
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
        assert_eq!(writes[2].1, "old-p");
    }

    #[test]
    fn every_read_state_has_a_distinct_name() {
        assert_eq!(ClipboardRead::Text("value".into()).state(), "Text");
        assert_eq!(ClipboardRead::Empty.state(), "Empty");
        assert_eq!(ClipboardRead::NonText.state(), "NonText");
        assert_eq!(ClipboardRead::Unsupported.state(), "Unsupported");
        assert_eq!(ClipboardRead::Error("failed".into()).state(), "Error");
    }

    #[test]
    fn copy_writes_only_confirmed_text() {
        for destination in [CopyClipboardType::Primary, CopyClipboardType::Regular] {
            let mut writes = Vec::new();
            copy_read_to(
                "COPY",
                "source",
                ClipboardRead::Text("value".into()),
                destination,
                false,
                |clipboard, value, _| {
                    writes.push((clipboard, value));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(writes, [(destination, "value".into())]);

            for state in unreadable_states() {
                writes.clear();
                assert!(
                    copy_read_to(
                        "COPY",
                        "source",
                        state,
                        destination,
                        false,
                        |clipboard, value, _| {
                            writes.push((clipboard, value));
                            Ok(())
                        },
                    )
                    .is_err()
                );
                assert!(writes.is_empty());
            }
        }
    }

    #[test]
    fn switch_writes_only_when_both_reads_are_text() {
        let mut writes = Vec::new();
        switch_reads(
            ClipboardRead::Text("primary".into()),
            ClipboardRead::Text("regular".into()),
            false,
            |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            writes,
            [
                (CopyClipboardType::Primary, "regular".into()),
                (CopyClipboardType::Regular, "primary".into()),
            ]
        );

        for state in unreadable_states() {
            writes.clear();
            assert!(
                switch_reads(
                    state.clone(),
                    ClipboardRead::Text("regular".into()),
                    false,
                    |clipboard, value, _| {
                        writes.push((clipboard, value));
                        Ok(())
                    },
                )
                .is_err()
            );
            assert!(writes.is_empty());

            assert!(
                switch_reads(
                    ClipboardRead::Text("primary".into()),
                    state,
                    false,
                    |clipboard, value, _| {
                        writes.push((clipboard, value));
                        Ok(())
                    },
                )
                .is_err()
            );
            assert!(writes.is_empty());
        }
    }

    #[test]
    fn actions_read_write_and_clear_the_expected_selections() {
        for (action, expected_source, expected_destination) in [
            (
                ClipboardAction::CopyRegular,
                ClipboardType::Regular,
                CopyClipboardType::Primary,
            ),
            (
                ClipboardAction::CopyPrimary,
                ClipboardType::Primary,
                CopyClipboardType::Regular,
            ),
        ] {
            let mut reads = Vec::new();
            let mut writes = Vec::new();
            apply_action(
                action,
                false,
                |clipboard| {
                    reads.push(clipboard);
                    ClipboardRead::Text("value".into())
                },
                |clipboard, value, _| {
                    writes.push((clipboard, value));
                    Ok(())
                },
                || panic!("copy must not clear clipboards"),
            )
            .unwrap();
            assert_eq!(reads, [expected_source]);
            assert_eq!(writes, [(expected_destination, "value".into())]);
        }

        let mut writes = Vec::new();
        apply_action(
            ClipboardAction::Switch,
            false,
            |clipboard| match clipboard {
                ClipboardType::Primary => ClipboardRead::Text("primary".into()),
                ClipboardType::Regular => ClipboardRead::Text("regular".into()),
            },
            |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            },
            || panic!("switch must not clear clipboards"),
        )
        .unwrap();
        assert_eq!(writes.len(), 2);

        let mut clears = 0;
        apply_action(
            ClipboardAction::Reset,
            false,
            |_| panic!("reset must not read clipboards"),
            |_, _, _| panic!("reset must not use the text writer"),
            || {
                clears += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(clears, 1);
    }
}

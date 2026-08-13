use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadLimits {
    pub(crate) max_bytes: u64,
    pub(crate) timeout: Duration,
}

/// Bounds a blocking read by a deadline.
///
/// The clipboard pipe is served by whichever application owns the selection, so an
/// unresponsive owner would otherwise stall the read — and the task awaiting it —
/// forever. Wrapping the future in a timeout cannot help, because that abandons the
/// future while the blocking read keeps the worker; the deadline has to be enforced
/// on the descriptor itself.
struct DeadlineReader<R> {
    reader: R,
    timeout: Duration,
    deadline: Instant,
}

impl<R: AsRawFd> DeadlineReader<R> {
    fn new(reader: R, timeout: Duration) -> io::Result<Self> {
        set_non_blocking(reader.as_raw_fd())?;
        Ok(Self {
            reader,
            timeout,
            deadline: Instant::now() + timeout,
        })
    }

    fn timed_out(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out after {} ms", self.timeout.as_millis()),
        )
    }
}

impl<R: Read + AsRawFd> Read for DeadlineReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.timed_out());
            }
            let mut descriptor = libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let milliseconds = remaining.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: `descriptor` holds one initialized descriptor that `reader` keeps
            // open across the call, and `poll` does not retain the pointer.
            let ready = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
            if ready == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                return Err(self.timed_out());
            }
            match self.reader.read(buffer) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        }
    }
}

fn set_non_blocking(descriptor: RawFd) -> io::Result<()> {
    // SAFETY: `descriptor` is borrowed from a live reader and `fcntl` does not retain it.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardRead {
    Text(String),
    Empty,
    NonText,
    Unsupported,
    Oversized { limit: u64 },
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SwitchValue {
    Text(String),
    Empty,
}

impl ClipboardRead {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::Text(_) => "Text",
            Self::Empty => "Empty",
            Self::NonText => "NonText",
            Self::Unsupported => "Unsupported",
            Self::Oversized { .. } => "Oversized",
            Self::Error(_) => "Error",
        }
    }

    pub(crate) fn text_length(&self) -> usize {
        match self {
            Self::Text(value) => value.chars().count(),
            _ => 0,
        }
    }

    pub(crate) fn observation(&self) -> Option<Option<String>> {
        match self {
            Self::Text(value) => Some(Some(value.clone())),
            Self::Empty => Some(None),
            Self::NonText | Self::Unsupported | Self::Oversized { .. } | Self::Error(_) => None,
        }
    }

    fn into_text(self, source: &str) -> Result<String, String> {
        match self {
            Self::Text(value) => Ok(value),
            Self::Empty => Err(format!("{source} clipboard is empty")),
            Self::NonText => Err(format!("{source} clipboard does not contain text")),
            Self::Unsupported => Err(format!("{source} clipboard is unsupported")),
            Self::Oversized { limit } => Err(format!(
                "{source} clipboard exceeds the configured {limit}-byte limit"
            )),
            Self::Error(error) => Err(format!("could not read {source} clipboard: {error}")),
        }
    }

    fn into_switch_value(self, source: &str) -> Result<SwitchValue, String> {
        match self {
            Self::Text(value) => Ok(SwitchValue::Text(value)),
            Self::Empty => Ok(SwitchValue::Empty),
            Self::NonText => Err(format!("{source} clipboard does not contain text")),
            Self::Unsupported => Err(format!("{source} clipboard is unsupported")),
            Self::Oversized { limit } => Err(format!(
                "{source} clipboard exceeds the configured {limit}-byte limit"
            )),
            Self::Error(error) => Err(format!("could not read {source} clipboard: {error}")),
        }
    }

    pub(crate) fn into_editable(self, source: &str) -> Result<String, String> {
        self.into_text(source)
    }
}

pub(crate) fn read_both(limits: ReadLimits) -> (ClipboardRead, ClipboardRead) {
    read_both_with(|clipboard| read(clipboard, limits))
}

fn read_both_with<F>(mut read: F) -> (ClipboardRead, ClipboardRead)
where
    F: FnMut(ClipboardType) -> ClipboardRead,
{
    (read(ClipboardType::Primary), read(ClipboardType::Regular))
}

pub(crate) fn read(clipboard: ClipboardType, limits: ReadLimits) -> ClipboardRead {
    let (pipe, _) = match get_contents(clipboard, Seat::Unspecified, MimeType::Text) {
        Ok(contents) => contents,
        Err(Error::ClipboardEmpty) => return ClipboardRead::Empty,
        Err(Error::NoMimeType) => return ClipboardRead::NonText,
        Err(Error::PrimarySelectionUnsupported) => return ClipboardRead::Unsupported,
        Err(error) => return ClipboardRead::Error(error.to_string()),
    };
    match DeadlineReader::new(pipe, limits.timeout) {
        Ok(reader) => read_text(reader, limits.max_bytes),
        Err(error) => ClipboardRead::Error(format!("could not prepare clipboard reader: {error}")),
    }
}

fn read_text(reader: impl Read, max_bytes: u64) -> ClipboardRead {
    let mut bytes = Vec::new();
    match reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() as u64 > max_bytes => ClipboardRead::Oversized { limit: max_bytes },
        // A text MIME type does not promise UTF-8; `text/plain;charset=ISO-8859-1` is
        // offered as text too. Substituting replacement characters would let COPY and
        // SWITCH write the mangled text back over the original, so refuse the selection
        // instead. Truncation cannot cause this: anything longer was already oversized.
        Ok(_) => match String::from_utf8(bytes) {
            Ok(value) => ClipboardRead::Text(value),
            Err(_) => ClipboardRead::NonText,
        },
        Err(error) => ClipboardRead::Error(format!("could not receive contents: {error}")),
    }
}

pub(crate) fn perform_action(
    action: ClipboardAction,
    limits: ReadLimits,
    notifications: bool,
    debug: bool,
) -> Result<(), String> {
    apply_action(
        action,
        debug,
        |clipboard| read(clipboard, limits),
        write,
        write_switch_value,
        || clear(CopyClipboardType::Both, CopySeat::All).map_err(|error| error.to_string()),
    )?;
    let notification_sent =
        notification::send_if_enabled(action_notification(action), "clipboard", notifications);
    if notification_sent && debug {
        eprintln!("[debug] notification sent: {}", action.name());
    } else if !notifications && debug {
        eprintln!("[debug] notification skipped: disabled");
    }
    Ok(())
}

fn apply_action<FRead, FWrite, FSwitchWrite, FClear>(
    action: ClipboardAction,
    debug: bool,
    mut read: FRead,
    mut write: FWrite,
    mut switch_write: FSwitchWrite,
    mut clear_both: FClear,
) -> Result<(), String>
where
    FRead: FnMut(ClipboardType) -> ClipboardRead,
    FWrite: FnMut(CopyClipboardType, String, bool) -> Result<(), String>,
    FSwitchWrite: FnMut(CopyClipboardType, SwitchValue, bool) -> Result<(), String>,
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
            &mut switch_write,
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
    F: FnMut(CopyClipboardType, SwitchValue, bool) -> Result<(), String>,
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
    let primary = primary.into_switch_value("primary")?;
    let regular = regular.into_switch_value("regular")?;
    if primary == SwitchValue::Empty && regular == SwitchValue::Empty {
        return Ok(());
    }
    write_both_with_rollback(regular, primary.clone(), primary, debug, write)
}

fn write_switch_value(
    clipboard: CopyClipboardType,
    value: SwitchValue,
    debug: bool,
) -> Result<(), String> {
    match value {
        SwitchValue::Text(value) => write(clipboard, value, debug),
        SwitchValue::Empty => {
            if debug {
                eprintln!("[debug] clearing switch destination: {clipboard:?}");
            }
            clear(clipboard, CopySeat::All).map_err(|error| error.to_string())
        }
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

pub(crate) fn write_both_with_rollback<T, F>(
    primary: T,
    regular: T,
    original_primary: T,
    debug: bool,
    mut write: F,
) -> Result<(), String>
where
    F: FnMut(CopyClipboardType, T, bool) -> Result<(), String>,
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
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    const BRIEF: Duration = Duration::from_millis(50);

    fn unreadable_states() -> Vec<ClipboardRead> {
        vec![
            ClipboardRead::Empty,
            ClipboardRead::NonText,
            ClipboardRead::Unsupported,
            ClipboardRead::Oversized { limit: 10 },
            ClipboardRead::Error("read failed".into()),
        ]
    }

    fn unsafe_switch_states() -> Vec<ClipboardRead> {
        vec![
            ClipboardRead::NonText,
            ClipboardRead::Unsupported,
            ClipboardRead::Oversized { limit: 10 },
            ClipboardRead::Error("read failed".into()),
        ]
    }

    #[test]
    fn pair_write_restores_primary_when_regular_fails() {
        let mut writes = Vec::new();
        let result = write_both_with_rollback(
            "new-p".into(),
            "new-r".into(),
            "old-p".into(),
            false,
            |clipboard, value: String, _| {
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
        assert_eq!(ClipboardRead::Oversized { limit: 10 }.state(), "Oversized");
        assert_eq!(ClipboardRead::Error("failed".into()).state(), "Error");
    }

    #[test]
    fn reads_each_selection_even_when_the_other_fails() {
        for (failed_selection, expected) in [
            (
                ClipboardType::Primary,
                (
                    ClipboardRead::Error("failed".into()),
                    ClipboardRead::Text("regular".into()),
                ),
            ),
            (
                ClipboardType::Regular,
                (
                    ClipboardRead::Text("primary".into()),
                    ClipboardRead::Error("failed".into()),
                ),
            ),
        ] {
            let mut reads = Vec::new();
            let actual = read_both_with(|selection| {
                reads.push(selection);
                if selection == failed_selection {
                    ClipboardRead::Error("failed".into())
                } else {
                    ClipboardRead::Text(
                        match selection {
                            ClipboardType::Primary => "primary",
                            ClipboardType::Regular => "regular",
                        }
                        .into(),
                    )
                }
            });
            assert_eq!(actual, expected);
            assert_eq!(reads, [ClipboardType::Primary, ClipboardType::Regular]);
        }

        let both_failed =
            read_both_with(|selection| ClipboardRead::Error(format!("{selection:?} failed")));
        assert!(matches!(both_failed.0, ClipboardRead::Error(_)));
        assert!(matches!(both_failed.1, ClipboardRead::Error(_)));
    }

    #[test]
    fn only_text_and_empty_are_change_observations() {
        assert_eq!(
            ClipboardRead::Text("value".into()).observation(),
            Some(Some("value".into()))
        );
        assert_eq!(ClipboardRead::Empty.observation(), Some(None));
        for state in unsafe_switch_states() {
            assert_eq!(state.observation(), None);
        }
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
    fn switch_exchanges_text_and_empty_states() {
        let mut writes = Vec::new();
        for (primary, regular, expected) in [
            (
                ClipboardRead::Text("primary".into()),
                ClipboardRead::Text("regular".into()),
                vec![
                    (
                        CopyClipboardType::Primary,
                        SwitchValue::Text("regular".into()),
                    ),
                    (
                        CopyClipboardType::Regular,
                        SwitchValue::Text("primary".into()),
                    ),
                ],
            ),
            (
                ClipboardRead::Text("primary".into()),
                ClipboardRead::Empty,
                vec![
                    (CopyClipboardType::Primary, SwitchValue::Empty),
                    (
                        CopyClipboardType::Regular,
                        SwitchValue::Text("primary".into()),
                    ),
                ],
            ),
            (
                ClipboardRead::Empty,
                ClipboardRead::Text("regular".into()),
                vec![
                    (
                        CopyClipboardType::Primary,
                        SwitchValue::Text("regular".into()),
                    ),
                    (CopyClipboardType::Regular, SwitchValue::Empty),
                ],
            ),
            (ClipboardRead::Empty, ClipboardRead::Empty, vec![]),
        ] {
            writes.clear();
            switch_reads(primary, regular, false, |clipboard, value, _| {
                writes.push((clipboard, value));
                Ok(())
            })
            .unwrap();
            assert_eq!(writes, expected);
        }
    }

    #[test]
    fn switch_does_not_mutate_unreadable_clipboards() {
        let mut writes = Vec::new();
        for state in unsafe_switch_states() {
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
    fn switch_restores_an_empty_primary_when_regular_clear_fails() {
        let mut writes = Vec::new();
        let result = switch_reads(
            ClipboardRead::Empty,
            ClipboardRead::Text("regular".into()),
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
        assert_eq!(writes[2], (CopyClipboardType::Primary, SwitchValue::Empty));
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
                |_, _, _| panic!("copy must not use the switch writer"),
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
            |_, _, _| panic!("switch must not use the text writer"),
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
            |_, _, _| panic!("reset must not use the switch writer"),
            || {
                clears += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(clears, 1);
    }

    #[test]
    fn a_silent_writer_does_not_stall_the_read_forever() {
        // The writer stays open and never sends, which is what a hung clipboard owner
        // looks like: without a deadline this read would never return.
        let (reader, _writer) = UnixStream::pair().unwrap();
        let mut reader = DeadlineReader::new(reader, BRIEF).unwrap();
        assert_eq!(
            reader.read(&mut [0; 8]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn a_timed_out_read_is_reported_as_an_error() {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let reader = DeadlineReader::new(reader, BRIEF).unwrap();
        let ClipboardRead::Error(error) = read_text(reader, 1024) else {
            panic!("a timed out read must not be mistaken for content");
        };
        assert!(error.contains("timed out"), "unexpected error: {error}");
    }

    #[test]
    fn a_stalled_writer_never_yields_partial_content() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"partial").unwrap();
        let reader = DeadlineReader::new(reader, BRIEF).unwrap();
        assert!(matches!(read_text(reader, 1024), ClipboardRead::Error(_)));
    }

    #[test]
    fn content_delivered_before_the_deadline_reads_normally() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all("é🙂 value".as_bytes()).unwrap();
        drop(writer);
        let reader = DeadlineReader::new(reader, Duration::from_secs(30)).unwrap();
        assert_eq!(
            read_text(reader, 1024),
            ClipboardRead::Text("é🙂 value".into())
        );
    }

    #[test]
    fn clipboard_read_limit_accepts_below_and_at_but_rejects_above() {
        assert_eq!(
            read_text("123".as_bytes(), 4),
            ClipboardRead::Text("123".into())
        );
        assert_eq!(
            read_text("1234".as_bytes(), 4),
            ClipboardRead::Text("1234".into())
        );
        assert_eq!(
            read_text("12345".as_bytes(), 4),
            ClipboardRead::Oversized { limit: 4 }
        );
    }

    #[test]
    fn invalid_utf8_is_refused_rather_than_repaired() {
        // "café" as a latin-1 `text/plain` selection would serve these bytes.
        assert_eq!(
            read_text(b"caf\xe9".as_slice(), 1024),
            ClipboardRead::NonText
        );
        assert_eq!(read_text([0xff].as_slice(), 1024), ClipboardRead::NonText);
        assert_eq!(
            read_text("café🙂".as_bytes(), 1024),
            ClipboardRead::Text("café🙂".into())
        );
    }

    #[test]
    fn a_latin1_selection_is_never_written_back() {
        let mut writes = Vec::new();
        assert!(
            copy_read_to(
                "COPY",
                "source",
                read_text(b"caf\xe9".as_slice(), 1024),
                CopyClipboardType::Primary,
                false,
                |clipboard, value, _| {
                    writes.push((clipboard, value));
                    Ok(())
                },
            )
            .is_err()
        );
        assert!(
            writes.is_empty(),
            "wrote replacement characters: {writes:?}"
        );
    }
}

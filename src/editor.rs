use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) async fn edit(
    command: &[String],
    original: &str,
    max_bytes: u64,
    debug: bool,
) -> Result<String, String> {
    validate_command(command)?;
    let file = prepare_file(original)?;
    log_started(original, debug);
    let mut child = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .arg(file.path())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not launch editor: {error}"))?;
    let status = child
        .wait()
        .await
        .map_err(|error| format!("could not wait for editor: {error}"))?;
    if !status.success() {
        return Err("editor exited unsuccessfully; original value preserved".into());
    }
    read_edited_value(file.path(), max_bytes, debug)
}

#[cfg(test)]
fn edit_with<F>(
    command: &[String],
    original: &str,
    max_bytes: u64,
    debug: bool,
    run: F,
) -> Result<String, String>
where
    F: FnOnce(&[String], &Path) -> Result<bool, String>,
{
    validate_command(command)?;
    let file = prepare_file(original)?;
    log_started(original, debug);
    if !run(command, file.path())? {
        return Err("editor exited unsuccessfully; original value preserved".into());
    }
    read_edited_value(file.path(), max_bytes, debug)
}

fn validate_command(command: &[String]) -> Result<(), String> {
    if command.is_empty() {
        Err("no editor command is configured".into())
    } else {
        Ok(())
    }
}

fn prepare_file(original: &str) -> Result<tempfile::NamedTempFile, String> {
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|error| format!("could not create private edit file: {error}"))?;
    file.write_all(original.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|error| format!("could not prepare edit file: {error}"))?;
    Ok(file)
}

fn log_started(original: &str, debug: bool) {
    if debug {
        eprintln!(
            "[debug] editor started: original_length={} chars",
            original.chars().count()
        );
    }
}

fn read_edited_value(path: &Path, max_bytes: u64, debug: bool) -> Result<String, String> {
    // Editors commonly save by writing a replacement file and renaming it over the
    // original, which leaves the prepared descriptor pointing at the old inode. Reopen
    // by path so those edits are not silently discarded, refusing to follow a symlink
    // because the path is no longer guaranteed to name the file we created.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            format!("could not reopen edited value: {error}; original value preserved")
        })?;
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect edited value: {error}"))?
        .len();
    if length > max_bytes.saturating_add(1) {
        return Err(format!(
            "edited value is too large ({length} bytes; limit is {max_bytes})"
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(max_bytes.saturating_add(2))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read edited value: {error}"))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "edited value is too large ({} bytes; limit is {max_bytes})",
            bytes.len()
        ));
    }
    let value = String::from_utf8(bytes)
        .map_err(|_| "edited value is not valid UTF-8; original value preserved".to_string())?;
    if debug {
        eprintln!(
            "[debug] editor completed: edited_length={} chars",
            value.chars().count()
        );
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 1024;

    #[test]
    fn successful_edit_returns_replacement() {
        let value = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            std::fs::write(path, "edited").map_err(|error| error.to_string())?;
            Ok(true)
        })
        .unwrap();
        assert_eq!(value, "edited");
    }

    #[test]
    fn edit_saved_by_rename_is_not_discarded() {
        let value = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            let replacement = path.with_extension("replacement");
            std::fs::write(&replacement, "edited\n").map_err(|error| error.to_string())?;
            std::fs::rename(&replacement, path).map_err(|error| error.to_string())?;
            Ok(true)
        })
        .unwrap();
        assert_eq!(value, "edited");
    }

    #[test]
    fn removed_edit_file_preserves_the_original() {
        let result = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
            Ok(true)
        });
        assert!(result.unwrap_err().contains("original value preserved"));
    }

    #[test]
    fn symlinked_edit_file_is_refused() {
        let mut target: Option<std::path::PathBuf> = None;
        let result = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            let link_target = path.with_extension("target");
            std::fs::write(&link_target, "attacker\n").map_err(|error| error.to_string())?;
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
            std::os::unix::fs::symlink(&link_target, path).map_err(|error| error.to_string())?;
            target = Some(link_target);
            Ok(true)
        });
        assert!(result.unwrap_err().contains("original value preserved"));
        std::fs::remove_file(target.unwrap()).unwrap();
    }

    #[test]
    fn edit_file_exposes_and_preserves_trailing_newline_state() {
        for (original, expected_file) in [
            ("without newline", "without newline\n"),
            ("with newline\n", "with newline\n\n"),
        ] {
            let value = edit_with(&["editor".into()], original, LIMIT, false, |_, path| {
                let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                assert_eq!(contents, expected_file);
                Ok(true)
            })
            .unwrap();
            assert_eq!(value, original);
        }
    }

    #[test]
    fn edited_file_controls_result_trailing_newline() {
        for (edited_file, expected) in [
            ("edited\n", "edited"),
            ("edited\n\n", "edited\n"),
            ("edited", "edited"),
        ] {
            let value = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
                std::fs::write(path, edited_file).map_err(|error| error.to_string())?;
                Ok(true)
            })
            .unwrap();
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn empty_output_is_valid() {
        let value = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            std::fs::write(path, "").map_err(|error| error.to_string())?;
            Ok(true)
        })
        .unwrap();
        assert!(value.is_empty());
    }

    #[test]
    fn failed_editor_preserves_original_by_returning_an_error() {
        assert!(
            edit_with(&["editor".into()], "original", LIMIT, false, |_, _| Ok(
                false
            ))
            .is_err()
        );
    }

    #[test]
    fn oversized_output_is_rejected() {
        let result = edit_with(&["editor".into()], "original", LIMIT, false, |_, path| {
            let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
            file.set_len(LIMIT + 2).map_err(|error| error.to_string())?;
            Ok(true)
        });
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn edited_output_limit_accepts_below_and_at_but_rejects_above() {
        for (edited, expected) in [("123\n", true), ("1234\n", true), ("12345\n", false)] {
            let result = edit_with(&["editor".into()], "original", 4, false, |_, path| {
                std::fs::write(path, edited).map_err(|error| error.to_string())?;
                Ok(true)
            });
            assert_eq!(result.is_ok(), expected);
        }
    }

    #[tokio::test]
    async fn asynchronous_editor_returns_replacement_and_failure() {
        let edited = edit(
            &[
                "sh".into(),
                "-c".into(),
                "printf edited > \"$1\"".into(),
                "editor".into(),
            ],
            "original",
            LIMIT,
            false,
        )
        .await
        .unwrap();
        assert_eq!(edited, "edited");

        assert!(
            edit(
                &["sh".into(), "-c".into(), "exit 1".into()],
                "original",
                LIMIT,
                false,
            )
            .await
            .unwrap_err()
            .contains("original value preserved")
        );
    }

    #[tokio::test]
    async fn asynchronous_editor_saving_by_rename_is_not_discarded() {
        let edited = edit(
            &[
                "sh".into(),
                "-c".into(),
                "printf edited > \"$1.new\" && mv \"$1.new\" \"$1\"".into(),
                "editor".into(),
            ],
            "original",
            LIMIT,
            false,
        )
        .await
        .unwrap();
        assert_eq!(edited, "edited");
    }

    #[tokio::test]
    async fn editor_task_can_be_cancelled_during_shutdown() {
        let task = tokio::spawn(async {
            edit(
                &["sh".into(), "-c".into(), "exec sleep 60".into()],
                "original",
                LIMIT,
                false,
            )
            .await
        });
        tokio::task::yield_now().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}

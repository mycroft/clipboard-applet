use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

const MAX_EDITED_BYTES: u64 = 1024 * 1024;

pub(crate) fn edit(command: &[String], original: &str, debug: bool) -> Result<String, String> {
    edit_with(command, original, debug, |command, path| {
        let status = Command::new(&command[0])
            .args(&command[1..])
            .arg(path)
            .status()
            .map_err(|error| format!("could not launch editor: {error}"))?;
        Ok(status.success())
    })
}

fn edit_with<F>(command: &[String], original: &str, debug: bool, run: F) -> Result<String, String>
where
    F: FnOnce(&[String], &Path) -> Result<bool, String>,
{
    if command.is_empty() {
        return Err("no editor command is configured".into());
    }
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|error| format!("could not create private edit file: {error}"))?;
    file.write_all(original.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|error| format!("could not prepare edit file: {error}"))?;
    if debug {
        eprintln!(
            "[debug] editor started: original_length={} chars",
            original.chars().count()
        );
    }
    if !run(command, file.path())? {
        return Err("editor exited unsuccessfully; original value preserved".into());
    }
    let length = file
        .as_file()
        .metadata()
        .map_err(|error| format!("could not inspect edited value: {error}"))?
        .len();
    if length > MAX_EDITED_BYTES + 1 {
        return Err(format!(
            "edited value is too large ({length} bytes; limit is {MAX_EDITED_BYTES})"
        ));
    }
    file.as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("could not rewind edited value: {error}"))?;
    let mut bytes = Vec::with_capacity(length as usize);
    file.as_file_mut()
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read edited value: {error}"))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() as u64 > MAX_EDITED_BYTES {
        return Err(format!(
            "edited value is too large ({} bytes; limit is {MAX_EDITED_BYTES})",
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

    #[test]
    fn successful_edit_returns_replacement() {
        let value = edit_with(&["editor".into()], "original", false, |_, path| {
            std::fs::write(path, "edited").map_err(|error| error.to_string())?;
            Ok(true)
        })
        .unwrap();
        assert_eq!(value, "edited");
    }

    #[test]
    fn edit_file_exposes_and_preserves_trailing_newline_state() {
        for (original, expected_file) in [
            ("without newline", "without newline\n"),
            ("with newline\n", "with newline\n\n"),
        ] {
            let value = edit_with(&["editor".into()], original, false, |_, path| {
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
            let value = edit_with(&["editor".into()], "original", false, |_, path| {
                std::fs::write(path, edited_file).map_err(|error| error.to_string())?;
                Ok(true)
            })
            .unwrap();
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn empty_output_is_valid() {
        let value = edit_with(&["editor".into()], "original", false, |_, path| {
            std::fs::write(path, "").map_err(|error| error.to_string())?;
            Ok(true)
        })
        .unwrap();
        assert!(value.is_empty());
    }

    #[test]
    fn failed_editor_preserves_original_by_returning_an_error() {
        assert!(edit_with(&["editor".into()], "original", false, |_, _| Ok(false)).is_err());
    }

    #[test]
    fn oversized_output_is_rejected() {
        let result = edit_with(&["editor".into()], "original", false, |_, path| {
            let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
            file.set_len(MAX_EDITED_BYTES + 1)
                .map_err(|error| error.to_string())?;
            Ok(true)
        });
        assert!(result.unwrap_err().contains("too large"));
    }
}

use std::ffi::OsString;
use std::path::PathBuf;

use crate::notification::NotificationMode;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CliAction {
    Run {
        config_file: Option<PathBuf>,
        debug: bool,
        with_notifications: Option<NotificationMode>,
    },
    Help,
}

pub(crate) fn print_help() {
    println!(
        "{name} {version}\n\nWayland clipboard tray applet\n\nUsage: {name} [OPTIONS]\n\nOptions:\n  -c, --config-file <PATH>            Use this configuration file\n  -d, --debug                         Log clipboard actions to stderr\n      --with-notifications <MODE>     Notification mode: true, false, or all\n  -h, --help                          Show this help",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION")
    );
}

pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<CliAction, String> {
    let mut args = args.into_iter();
    let mut config_file = None;
    let mut debug = false;
    let mut with_notifications = None;
    while let Some(argument) = args.next() {
        let value = match argument.to_str() {
            Some("-h" | "--help") => return Ok(CliAction::Help),
            Some("-d" | "--debug") => {
                debug = true;
                continue;
            }
            Some("--with-notifications") => {
                let value = args.next().ok_or_else(|| {
                    "--with-notifications requires true, false, or all".to_string()
                })?;
                let value = value.to_str().ok_or_else(|| {
                    "--with-notifications requires true, false, or all".to_string()
                })?;
                if with_notifications
                    .replace(parse_notification_mode(value)?)
                    .is_some()
                {
                    return Err("--with-notifications specified more than once".into());
                }
                continue;
            }
            Some(argument) if argument.starts_with("--with-notifications=") => {
                let value = &argument["--with-notifications=".len()..];
                if with_notifications
                    .replace(parse_notification_mode(value)?)
                    .is_some()
                {
                    return Err("--with-notifications specified more than once".into());
                }
                continue;
            }
            Some("-c" | "--config-file") => PathBuf::from(
                args.next()
                    .ok_or_else(|| format!("{} requires a path", argument.to_string_lossy()))?,
            ),
            Some(argument) if argument.starts_with("--config-file=") => {
                let path = &argument["--config-file=".len()..];
                if path.is_empty() {
                    return Err("--config-file requires a path".into());
                }
                path.into()
            }
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy())),
        };
        if config_file.replace(value).is_some() {
            return Err("configuration file specified more than once".into());
        }
    }
    Ok(CliAction::Run {
        config_file,
        debug,
        with_notifications,
    })
}

fn parse_notification_mode(value: &str) -> Result<NotificationMode, String> {
    match value {
        "true" => Ok(NotificationMode::Enabled),
        "false" => Ok(NotificationMode::Disabled),
        "all" => Ok(NotificationMode::All),
        _ => Err(format!(
            "invalid --with-notifications value {value:?}; expected true, false, or all"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<CliAction, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_config_file_forms() {
        for args in [vec!["-c", "custom.toml"], vec!["--config-file=custom.toml"]] {
            assert_eq!(
                run(&args),
                Ok(CliAction::Run {
                    config_file: Some(PathBuf::from("custom.toml")),
                    debug: false,
                    with_notifications: None,
                })
            );
        }
    }

    #[test]
    fn parses_help_and_debug() {
        assert_eq!(run(&["-h"]), Ok(CliAction::Help));
        assert_eq!(run(&["--help"]), Ok(CliAction::Help));
        assert!(matches!(
            run(&["-d"]),
            Ok(CliAction::Run { debug: true, .. })
        ));
    }

    #[test]
    fn parses_notification_modes() {
        for (value, expected) in [
            ("true", NotificationMode::Enabled),
            ("false", NotificationMode::Disabled),
            ("all", NotificationMode::All),
        ] {
            assert!(matches!(
                run(&[&format!("--with-notifications={value}")]),
                Ok(CliAction::Run { with_notifications: Some(mode), .. }) if mode == expected
            ));
        }
    }

    #[test]
    fn rejects_missing_invalid_and_duplicate_values() {
        assert!(run(&["--with-notifications"]).is_err());
        assert!(run(&["--with-notifications=maybe"]).is_err());
        assert!(run(&["--with-notifications=true", "--with-notifications=all"]).is_err());
        assert!(run(&["-c"]).is_err());
        assert!(run(&["-c", "one", "-c", "two"]).is_err());
    }
}

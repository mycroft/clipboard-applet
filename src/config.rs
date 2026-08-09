use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::clipboard::ClipboardAction;

const DEFAULT_POLLING_PERIOD_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum UpdateMethod {
    Events,
    Polling,
}

impl UpdateMethod {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Events => "EVENTS",
            Self::Polling => "POLLING",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) update_method: UpdateMethod,
    pub(crate) polling_period_ms: u64,
    pub(crate) hide_content: bool,
    pub(crate) notifications: bool,
    pub(crate) notify_on_change: bool,
    pub(crate) icon_name: String,
    pub(crate) editor: Vec<String>,
    pub(crate) stack_size: usize,
    pub(crate) left_click: ClipboardAction,
    pub(crate) middle_click: ClipboardAction,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            update_method: UpdateMethod::Events,
            polling_period_ms: DEFAULT_POLLING_PERIOD_MS,
            hide_content: false,
            notifications: false,
            notify_on_change: false,
            icon_name: "edit-paste".into(),
            editor: Vec::new(),
            stack_size: 16,
            left_click: ClipboardAction::CopyPrimary,
            middle_click: ClipboardAction::Switch,
        }
    }
}

pub(crate) fn load(config_file: Option<&Path>) -> Result<Config, String> {
    if let Some(path) = config_file {
        return load_from(path, false);
    }
    let Some(path) = path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    ) else {
        return Ok(Config::default());
    };
    load_from(&path, true)
}

fn path(
    xdg_config_home: Option<impl Into<PathBuf>>,
    home: Option<impl Into<PathBuf>>,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(Into::into)
        .or_else(|| home.map(|path| path.into().join(".config")))?;
    Some(base.join("clipboard-applet/config.toml"))
}

fn load_from(path: &Path, use_default_if_missing: bool) -> Result<Config, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if use_default_if_missing && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    parse(&contents, path)
}

fn parse(contents: &str, path: &Path) -> Result<Config, String> {
    let config: Config =
        toml::from_str(contents).map_err(|error| format!("{}: {error}", path.display()))?;
    if config.polling_period_ms == 0 {
        return Err(format!(
            "{}: polling_period_ms must be greater than zero",
            path.display()
        ));
    }
    if config.icon_name.trim().is_empty() {
        return Err(format!("{}: icon_name must not be empty", path.display()));
    }
    if config
        .editor
        .first()
        .is_some_and(|program| program.trim().is_empty())
    {
        return Err(format!(
            "{}: editor program must not be empty",
            path.display()
        ));
    }
    if !(1..=16).contains(&config.stack_size) {
        return Err(format!(
            "{}: stack_size must be between 1 and 16",
            path.display()
        ));
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_xdg_path_and_falls_back_to_home() {
        assert_eq!(
            path(Some("/xdg"), Some("/home/user")),
            Some(PathBuf::from("/xdg/clipboard-applet/config.toml"))
        );
        assert_eq!(
            path(None::<&str>, Some("/home/user")),
            Some(PathBuf::from(
                "/home/user/.config/clipboard-applet/config.toml"
            ))
        );
    }

    #[test]
    fn parses_values_and_applies_defaults() {
        let config = parse("polling_period_ms = 250", Path::new("config.toml")).unwrap();
        assert_eq!(config.polling_period_ms, 250);
        assert_eq!(config.update_method, UpdateMethod::Events);
        assert_eq!(config.left_click, ClipboardAction::CopyPrimary);
        assert_eq!(config.stack_size, 16);
        assert!(config.editor.is_empty());
    }

    #[test]
    fn parses_update_and_click_actions() {
        let config = parse(
            "update_method = 'POLLING'\nleft_click = 'RESET'\nmiddle_click = 'COPY_REGULAR'",
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(config.update_method, UpdateMethod::Polling);
        assert_eq!(config.left_click, ClipboardAction::Reset);
        assert_eq!(config.middle_click, ClipboardAction::CopyRegular);
    }

    #[test]
    fn validates_poll_period_icon_and_stack_size() {
        assert!(parse("polling_period_ms = 0", Path::new("config.toml")).is_err());
        assert!(parse("icon_name = '  '", Path::new("config.toml")).is_err());
        assert!(parse("stack_size = 0", Path::new("config.toml")).is_err());
        assert!(parse("stack_size = 17", Path::new("config.toml")).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(parse("surprise = true", Path::new("config.toml")).is_err());
    }

    #[test]
    fn parses_and_validates_editor_argv() {
        let config = parse("editor = ['foot', '-e', 'nvim']", Path::new("config.toml")).unwrap();
        assert_eq!(config.editor, ["foot", "-e", "nvim"]);
        assert!(parse("editor = ['  ']", Path::new("config.toml")).is_err());
    }
}

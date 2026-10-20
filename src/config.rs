use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use urushi_terminal::TerminalBackground;

use crate::{GraphicsPreference, app::SearchCase, theme};

const APP_DIRECTORY: &str = "yomu";
const CONFIG_FILE: &str = "config.toml";
const DEFAULT_MAX_WIDTH: u16 = 100;
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default)]
    theme_overrides: theme::ThemeSettings,
    #[serde(default)]
    view: ViewConfig,
    #[serde(default)]
    graphics: GraphicsConfig,
    #[serde(default)]
    search: SearchConfig,
    #[serde(skip)]
    theme_directory: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            theme_overrides: theme::ThemeSettings::default(),
            view: ViewConfig::default(),
            graphics: GraphicsConfig::default(),
            search: SearchConfig::default(),
            theme_directory: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewConfig {
    #[serde(default = "default_max_width")]
    max_width: u16,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            max_width: default_max_width(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphicsConfig {
    #[serde(default)]
    renderer: Option<GraphicsPreference>,
    #[serde(default)]
    experimental_sixel: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchConfig {
    #[serde(default)]
    case: SearchCase,
}

impl Config {
    pub(crate) fn load() -> Result<Self> {
        let Some(path) = config_path(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME")) else {
            return Ok(Self::default());
        };
        Self::load_from(&path)
    }

    fn load_from(path: &Path) -> Result<Self> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Self {
                    theme_directory: path.parent().map(|parent| parent.join("themes")),
                    ..Self::default()
                });
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read config file {}", path.display()));
            }
        };
        let mut config = toml::from_str::<Self>(&source)
            .with_context(|| format!("could not parse config file {}", path.display()))?;
        config.theme_directory = path.parent().map(|parent| parent.join("themes"));
        Ok(config)
    }

    pub(crate) fn body_max_width(&self) -> Option<u16> {
        (self.view.max_width > 0).then_some(self.view.max_width)
    }

    pub(crate) fn graphics_preference(&self) -> GraphicsPreference {
        match self.graphics.renderer {
            Some(renderer) => renderer,
            None if self.graphics.experimental_sixel => GraphicsPreference::Sixel,
            None => GraphicsPreference::Auto,
        }
    }

    pub(crate) fn search_case(&self) -> SearchCase {
        self.search.case
    }

    pub(crate) fn needs_terminal_background(&self, cli_theme: Option<&str>) -> bool {
        theme::needs_terminal_background(cli_theme.unwrap_or(&self.theme))
    }

    pub(crate) fn resolve_theme(
        &self,
        cli_theme: Option<&str>,
        background: Option<TerminalBackground>,
    ) -> Result<theme::Theme> {
        theme::resolve(
            cli_theme.unwrap_or(&self.theme),
            &self.theme_overrides,
            self.theme_directory.as_deref(),
            background,
        )
    }
}

fn default_theme() -> String {
    "builtin:auto".to_owned()
}

const fn default_max_width() -> u16 {
    DEFAULT_MAX_WIDTH
}

fn config_path(
    xdg_config_home: Option<impl AsRef<OsStr>>,
    home: Option<impl AsRef<OsStr>>,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(|value| PathBuf::from(value.as_ref()))
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|value| PathBuf::from(value.as_ref()).join(".config")))?;
    Some(base.join(APP_DIRECTORY).join(CONFIG_FILE))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn documented_config_example_parses() {
        toml::from_str::<Config>(include_str!("../examples/config.toml"))
            .expect("examples/config.toml should remain a valid Config");
    }

    #[test]
    fn xdg_config_home_takes_precedence() {
        assert_eq!(
            config_path(Some("/tmp/xdg"), Some("/home/user")),
            Some(PathBuf::from("/tmp/xdg/yomu/config.toml"))
        );
    }

    #[test]
    fn relative_xdg_config_home_falls_back_to_home() {
        assert_eq!(
            config_path(Some("relative"), Some("/home/user")),
            Some(PathBuf::from("/home/user/.config/yomu/config.toml"))
        );
    }

    #[test]
    fn view_width_defaults_to_one_hundred_cells() {
        let config = toml::from_str::<Config>("").expect("empty config should parse");

        assert_eq!(config.body_max_width(), Some(100));
    }

    #[test]
    fn view_width_accepts_custom_and_unbounded_values() {
        let custom = toml::from_str::<Config>("[view]\nmax_width = 72\n")
            .expect("custom view width should parse");
        let unbounded = toml::from_str::<Config>("[view]\nmax_width = 0\n")
            .expect("unbounded view width should parse");

        assert_eq!(custom.body_max_width(), Some(72));
        assert_eq!(unbounded.body_max_width(), None);
    }

    #[test]
    fn invalid_view_width_is_rejected() {
        let error = toml::from_str::<Config>("[view]\nmax_width = -1\n")
            .expect_err("negative view width should be rejected");

        assert!(error.to_string().contains("max_width"));
    }

    #[test]
    fn graphics_backend_is_loaded() {
        let config = toml::from_str::<Config>("[graphics]\nrenderer = \"text\"\n")
            .expect("graphics backend should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Text);
    }

    #[test]
    fn legacy_experimental_sixel_switch_remains_compatible() {
        let config = toml::from_str::<Config>("[graphics]\nexperimental_sixel = true\n")
            .expect("sixel override should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Sixel);
    }

    #[test]
    fn explicit_auto_takes_precedence_over_legacy_sixel_switch() {
        let config = toml::from_str::<Config>(
            "[graphics]\nrenderer = \"auto\"\nexperimental_sixel = true\n",
        )
        .expect("combined graphics settings should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Auto);
    }

    #[test]
    fn graphics_backend_defaults_to_auto() {
        let config = toml::from_str::<Config>("").expect("empty config should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Auto);
    }

    #[test]
    fn search_case_defaults_to_smart() {
        let config = toml::from_str::<Config>("").expect("empty config should parse");

        assert_eq!(config.search_case(), SearchCase::Smart);
    }

    #[test]
    fn search_case_accepts_each_supported_mode() {
        for (value, expected) in [
            ("smart", SearchCase::Smart),
            ("sensitive", SearchCase::Sensitive),
            ("insensitive", SearchCase::Insensitive),
        ] {
            let config = toml::from_str::<Config>(&format!("[search]\ncase = \"{value}\"\n"))
                .expect("documented search case should parse");

            assert_eq!(config.search_case(), expected);
        }
    }

    #[test]
    fn cli_theme_selection_keeps_config_overrides() {
        let config = toml::from_str::<Config>(
            "theme = \"builtin:light\"\n\n[theme_overrides.palette]\naccent = \"#ff87d7\"\n",
        )
        .expect("Theme config should parse");

        let theme = config
            .resolve_theme(Some("builtin:dark"), None)
            .expect("CLI-selected Theme should resolve");

        assert_eq!(theme.name, "builtin:dark");
        let theme::ColorChoice::Set(header) = theme.app.header.foreground else {
            panic!("header should have a foreground color");
        };
        assert_eq!(header.rgb, [0xff, 0x87, 0xd7]);
    }

    #[test]
    fn config_load_discovers_user_themes_next_to_config_file() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("yomu-config-theme-{suffix}"));
        let themes = root.join("themes");
        fs::create_dir_all(&themes).expect("Theme fixture directory should be writable");
        let config_path = root.join(CONFIG_FILE);
        fs::write(&config_path, "theme = \"paper\"\n").expect("config fixture should be writable");
        fs::write(themes.join("paper.toml"), "extends = \"builtin:light\"\n")
            .expect("Theme fixture should be writable");

        let config = Config::load_from(&config_path).expect("config should load");
        let theme = config
            .resolve_theme(None, None)
            .expect("discovered Theme should resolve");

        fs::remove_dir_all(root).expect("fixture directory should be removable");
        assert_eq!(theme.name, "paper");
    }
}

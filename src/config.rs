use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::GraphicsPreference;

const APP_DIRECTORY: &str = "raden";
const CONFIG_FILE: &str = "config.toml";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    heading: HeadingConfig,
    #[serde(default)]
    graphics: GraphicsConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadingConfig {
    #[serde(default)]
    families: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphicsConfig {
    #[serde(default)]
    backend: Option<GraphicsPreference>,
    #[serde(default)]
    experimental_sixel: bool,
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
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read config file {}", path.display()));
            }
        };
        let config = toml::from_str::<Self>(&source)
            .with_context(|| format!("could not parse config file {}", path.display()))?;
        Ok(config)
    }

    pub(crate) fn heading_families(&self) -> &[String] {
        &self.heading.families
    }

    pub(crate) fn graphics_preference(&self) -> GraphicsPreference {
        match self.graphics.backend {
            Some(backend) => backend,
            None if self.graphics.experimental_sixel => GraphicsPreference::Sixel,
            None => GraphicsPreference::Auto,
        }
    }
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
    fn xdg_config_home_takes_precedence() {
        assert_eq!(
            config_path(Some("/tmp/xdg"), Some("/home/user")),
            Some(PathBuf::from("/tmp/xdg/raden/config.toml"))
        );
    }

    #[test]
    fn relative_xdg_config_home_falls_back_to_home() {
        assert_eq!(
            config_path(Some("relative"), Some("/home/user")),
            Some(PathBuf::from("/home/user/.config/raden/config.toml"))
        );
    }

    #[test]
    fn heading_families_are_loaded_in_order() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("raden-config-{suffix}"));
        fs::create_dir_all(&root).expect("fixture directory should be writable");
        let path = root.join(CONFIG_FILE);
        fs::write(
            &path,
            "[heading]\nfamilies = [\"UDEV Gothic NF\", \"HackGen35 Console NF\"]\n",
        )
        .expect("fixture config should be writable");

        let config = Config::load_from(&path).expect("config should load");

        fs::remove_dir_all(&root).expect("fixture directory should be removable");
        assert_eq!(
            config.heading_families(),
            &["UDEV Gothic NF", "HackGen35 Console NF"]
        );
    }

    #[test]
    fn graphics_backend_is_loaded() {
        let config = toml::from_str::<Config>("[graphics]\nbackend = \"text\"\n")
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
        let config =
            toml::from_str::<Config>("[graphics]\nbackend = \"auto\"\nexperimental_sixel = true\n")
                .expect("combined graphics settings should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Auto);
    }

    #[test]
    fn graphics_backend_defaults_to_auto() {
        let config = toml::from_str::<Config>("").expect("empty config should parse");

        assert_eq!(config.graphics_preference(), GraphicsPreference::Auto);
    }
}

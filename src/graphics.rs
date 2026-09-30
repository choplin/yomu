use clap::ValueEnum;
use serde::Deserialize;

/// User-selected terminal graphics policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsPreference {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Text,
}

impl From<GraphicsPreference> for urushi_tui_app::GraphicsPreference {
    fn from(preference: GraphicsPreference) -> Self {
        match preference {
            GraphicsPreference::Auto => Self::Auto,
            GraphicsPreference::Kitty => Self::Kitty,
            GraphicsPreference::Sixel => Self::Sixel,
            GraphicsPreference::Text => Self::Text,
        }
    }
}

//! Parse a Velnopt configuration. No file access and no side effects.
//!
//! The schema is experimental. Only version 1 is accepted for now.
//!
//! ```
//! let config = velnopt_config::parse(
//!     "version = 1\n[panel]\nedge = 'top'\nheight = 32\n",
//! )?;
//! assert_eq!(config.panel().height().get(), 32);
//! # Ok::<(), velnopt_config::ConfigError>(())
//! ```

use std::{error::Error, fmt, num::NonZeroU16};

use serde::Deserialize;

/// Maximum supported configuration size in bytes (1 MiB).
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// A configuration that passed syntax, schema, and version checks.
#[derive(Clone, Debug)]
pub struct Config {
    panel: Panel,
}

impl Config {
    /// The validated panel settings.
    pub fn panel(&self) -> &Panel {
        &self.panel
    }
}

/// Settings for a single horizontal panel, in logical pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    edge: Edge,
    height: NonZeroU16,
    #[serde(default)]
    appearance: Appearance,
    #[serde(default = "default_widgets")]
    widgets: Vec<Widget>,
    #[serde(default)]
    output: Option<String>,
}

impl Panel {
    /// The screen edge the panel sits on.
    pub fn edge(&self) -> Edge {
        self.edge
    }

    /// Panel height in logical pixels. Never zero.
    pub fn height(&self) -> NonZeroU16 {
        self.height
    }

    /// Styling shared by all widgets.
    pub fn appearance(&self) -> &Appearance {
        &self.appearance
    }

    /// Widgets in declaration order, which also holds within each position.
    pub fn widgets(&self) -> &[Widget] {
        &self.widgets
    }

    /// An exact Wayland output name, or all connected outputs when absent.
    pub fn output(&self) -> Option<&str> {
        self.output.as_deref()
    }
}

/// Opaque sRGB color. Only the strict `#RRGGBB` form parses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color([u8; 3]);

impl Color {
    /// Red, green, and blue components.
    pub fn rgb(self) -> [u8; 3] {
        self.0
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 7
            || !text.starts_with('#')
            || !text[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(serde::de::Error::custom("color must be #RRGGBB"));
        }
        let value = u32::from_str_radix(&text[1..], 16).map_err(serde::de::Error::custom)?;
        Ok(Self([(value >> 16) as u8, (value >> 8) as u8, value as u8]))
    }
}

/// Panel typography and spacing, in logical pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    background: Color,
    foreground: Color,
    font: String,
    font_size: u16,
    padding: u16,
    gap: u16,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            background: Color([0x15, 0x2c, 0x2b]),
            foreground: Color([0xe4, 0xed, 0xe6]),
            font: "sans-serif".into(),
            font_size: 14,
            padding: 16,
            gap: 24,
        }
    }
}

impl Appearance {
    /// Opaque panel background.
    pub fn background(&self) -> Color {
        self.background
    }
    /// Default text color.
    pub fn foreground(&self) -> Color {
        self.foreground
    }
    /// System font family, including generic `sans-serif`, `serif`, or `monospace`.
    pub fn font(&self) -> &str {
        &self.font
    }
    /// Font size between 4 and 128 logical pixels.
    pub fn font_size(&self) -> u16 {
        self.font_size
    }
    /// Horizontal inset, at most 256 logical pixels.
    pub fn padding(&self) -> u16 {
        self.padding
    }
    /// Space between widgets, at most 256 logical pixels.
    pub fn gap(&self) -> u16 {
        self.gap
    }
}

/// Horizontal widget group. The sides keep their natural width and the center gets the rest.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    /// Start of the panel.
    #[default]
    Left,
    /// Center of the panel.
    Center,
    /// End of the panel.
    Right,
}

/// Panel widgets. A configuration can never run code or commands: interactive
/// widgets only do their fixed built-in action, and only on explicit user input.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Widget {
    /// A single line of shaped Unicode text.
    Text {
        /// Text to display, at most 1024 bytes without control characters.
        text: String,
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Current time as `HH:MM`, updated at minute boundaries.
    Clock {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
        /// Use UTC and include its label, instead of the system's local timezone.
        #[serde(default)]
        utc: bool,
    },
    /// Live Hyprland workspace names, with the focused workspace in brackets.
    Workspaces {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Title of Hyprland's focused window, or an explicit empty/offline state.
    #[serde(rename = "window-title")]
    WindowTitle {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Network connection status from NetworkManager. Display-only.
    Network {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Bluetooth adapter status from BlueZ; activating it toggles adapter power.
    Bluetooth {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Default audio output volume; activating it toggles mute, scrolling adjusts volume.
    Volume {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Battery charge and charging state from UPower. Display-only.
    Battery {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// A button that opens the searchable application launcher.
    Launcher {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
    /// Status notifier (system tray) items with their menus.
    Tray {
        /// Horizontal group, defaulting to left.
        #[serde(default)]
        position: Position,
        /// Optional text color override.
        #[serde(default)]
        color: Option<Color>,
    },
}

impl Widget {
    /// The horizontal group this widget sits in.
    pub fn position(&self) -> Position {
        match self {
            Self::Text { position, .. }
            | Self::Clock { position, .. }
            | Self::Workspaces { position, .. }
            | Self::WindowTitle { position, .. }
            | Self::Network { position, .. }
            | Self::Bluetooth { position, .. }
            | Self::Volume { position, .. }
            | Self::Battery { position, .. }
            | Self::Launcher { position, .. }
            | Self::Tray { position, .. } => *position,
        }
    }
    /// The text color override, if there is one.
    pub fn color(&self) -> Option<Color> {
        match self {
            Self::Text { color, .. }
            | Self::Clock { color, .. }
            | Self::Workspaces { color, .. }
            | Self::WindowTitle { color, .. }
            | Self::Network { color, .. }
            | Self::Bluetooth { color, .. }
            | Self::Volume { color, .. }
            | Self::Battery { color, .. }
            | Self::Launcher { color, .. }
            | Self::Tray { color, .. } => *color,
        }
    }
}

fn default_widgets() -> Vec<Widget> {
    vec![
        Widget::Text {
            text: "Velnopt".into(),
            position: Position::Left,
            color: Some(Color([0x96, 0xd1, 0xbd])),
        },
        Widget::Clock {
            position: Position::Right,
            color: None,
            utc: false,
        },
    ]
}

/// A horizontal screen edge. The first schema supports only these two.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    /// The top of the output.
    Top,
    /// The bottom of the output.
    Bottom,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    panel: Panel,
}

/// Why a configuration failed validation.
#[derive(Debug)]
pub enum ConfigError {
    /// The input exceeded [`MAX_CONFIG_BYTES`].
    TooLarge,
    /// TOML syntax or schema validation failed.
    Parse(toml::de::Error),
    /// The configuration requests an unsupported schema version.
    UnsupportedVersion(u32),
    /// A field parses fine but breaks one of the configuration limits.
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(formatter, "configuration exceeds {MAX_CONFIG_BYTES} bytes"),
            Self::Parse(error) => write!(formatter, "{error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported configuration version {version}; expected 1"
                )
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::TooLarge | Self::UnsupportedVersion(_) | Self::Invalid(_) => None,
        }
    }
}

/// Parse a version 1 configuration. Required fields must be present and unknown keys are rejected.
///
/// # Errors
///
/// Returns an error for oversized input, malformed TOML, invalid or missing
/// fields, and unsupported schema versions. Heights must be in `1..=65535`.
pub fn parse(source: &str) -> Result<Config, ConfigError> {
    if source.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }

    let document: Document = toml::from_str(source).map_err(ConfigError::Parse)?;
    if document.version != 1 {
        return Err(ConfigError::UnsupportedVersion(document.version));
    }

    let panel = &document.panel;
    let appearance = panel.appearance();
    if !(4..=128).contains(&appearance.font_size) {
        return Err(ConfigError::Invalid(
            "panel.appearance.font_size must be between 4 and 128".into(),
        ));
    }
    if appearance.padding > 256 || appearance.gap > 256 {
        return Err(ConfigError::Invalid(
            "panel.appearance padding and gap must be at most 256".into(),
        ));
    }
    if appearance.font.trim().is_empty()
        || appearance.font.len() > 128
        || appearance.font.chars().any(char::is_control)
    {
        return Err(ConfigError::Invalid(
            "panel.appearance.font must contain 1–128 bytes without control characters".into(),
        ));
    }
    if let Some(output) = &panel.output
        && (output.is_empty() || output.len() > 256 || output.chars().any(char::is_control))
    {
        return Err(ConfigError::Invalid(
            "panel.output must contain 1–256 bytes without control characters".into(),
        ));
    }
    if panel.widgets.len() > 24 {
        return Err(ConfigError::Invalid(
            "panel.widgets accepts at most 24 widgets".into(),
        ));
    }
    for widget in &panel.widgets {
        if let Widget::Text { text, .. } = widget
            && (text.len() > 1024 || text.chars().any(char::is_control))
        {
            return Err(ConfigError::Invalid(
                "widget text must be at most 1024 bytes without control characters".into(),
            ));
        }
    }

    Ok(Config {
        panel: document.panel,
    })
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, Edge, MAX_CONFIG_BYTES, parse};

    const EXAMPLE: &str = include_str!("../../../examples/panel.toml");

    #[test]
    fn accepts_documented_example() {
        let config = parse(EXAMPLE).expect("the shipped example must remain valid");
        assert_eq!(config.panel().edge(), Edge::Top);
        assert_eq!(config.panel().height().get(), 32);
    }

    #[test]
    fn accepts_desktop_example() {
        let config = parse(include_str!("../../../examples/desktop.toml"))
            .expect("the desktop example must remain valid");
        assert_eq!(config.panel().widgets().len(), 9);
    }

    #[test]
    fn accepts_both_edges_and_height_boundaries() {
        for edge in ["top", "bottom"] {
            for height in [1, 65535] {
                let source = format!("version = 1\n[panel]\nedge = '{edge}'\nheight = {height}");
                assert!(parse(&source).is_ok(), "{source}");
            }
        }
    }

    #[test]
    fn rejects_invalid_schema_and_syntax() {
        for source in [
            "",
            "version = [",
            "version = 1",
            "[panel]\nedge = 'top'\nheight = 32",
            "version = 1\n[panel]\nheight = 32",
            "version = 1\n[panel]\nedge = 'top'",
            "version = 1\n[panel]\nedge = 'left'\nheight = 32",
            "version = 1\n[panel]\nedge = 'top'\nheight = 0",
            "version = 1\n[panel]\nedge = 'top'\nheight = -1",
            "version = 1\n[panel]\nedge = 'top'\nheight = 65536",
            "version = 1\n[panel]\nedge = 'top'\nheight = 1.5",
            "version = 1\n[panel]\nedge = 'top'\nheight = '32'",
            "version = 1\nunknown = true\n[panel]\nedge = 'top'\nheight = 32",
            "version = 1\n[panel]\nedge = 'top'\nheight = 32\nunknown = true",
            "version = 1\n[panel]\nedge = 'top'\nheight = 32\nheight = 33",
        ] {
            assert!(
                matches!(parse(source), Err(ConfigError::Parse(_))),
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_versions() {
        for version in [0, 2, u32::MAX] {
            let source = format!("version = {version}\n[panel]\nedge = 'top'\nheight = 32");
            assert!(matches!(
                parse(&source),
                Err(ConfigError::UnsupportedVersion(value)) if value == version
            ));
        }
    }

    #[test]
    fn enforces_size_limit_at_boundary() {
        let mut source = EXAMPLE.to_owned();
        source.push('#');
        source.extend(std::iter::repeat_n(' ', MAX_CONFIG_BYTES - source.len()));
        assert!(parse(&source).is_ok());
        source.push(' ');
        assert!(matches!(parse(&source), Err(ConfigError::TooLarge)));
    }

    #[test]
    fn accepts_full_example_and_preserves_widget_order() {
        let config =
            parse(include_str!("../../../examples/hyprland.toml")).expect("full example is valid");
        assert_eq!(config.panel().widgets().len(), 3);
        assert_eq!(
            config.panel().widgets()[0].position(),
            super::Position::Left
        );
        assert_eq!(
            config.panel().widgets()[2].position(),
            super::Position::Right
        );
        assert_eq!(
            config.panel().appearance().background().rgb(),
            [0x15, 0x2c, 0x2b]
        );
    }

    #[test]
    fn accepts_live_hyprland_widgets_without_requiring_a_session() {
        let config = parse(include_str!("../../../examples/live-hyprland.toml"))
            .expect("live example is valid");
        assert!(matches!(
            config.panel().widgets()[0],
            super::Widget::Workspaces { .. }
        ));
        assert!(matches!(
            config.panel().widgets()[1],
            super::Widget::WindowTitle { .. }
        ));
        for kind in [
            "network",
            "bluetooth",
            "volume",
            "battery",
            "launcher",
            "tray",
        ] {
            let source = format!(
                "version=1\n[panel]\nedge='top'\nheight=32\n[[panel.widgets]]\nkind='{kind}'\nposition='right'\ncolor='#102030'"
            );
            let config = parse(&source).expect("service widget kinds parse without a session");
            assert_eq!(
                config.panel().widgets()[0].position(),
                super::Position::Right
            );
            assert!(config.panel().widgets()[0].color().is_some());
            // service widgets take no commands, scripts, or extra fields
            for extra in ["command='true'", "exec='sh'", "script='x'"] {
                assert!(
                    parse(&format!("{source}\n{extra}")).is_err(),
                    "{kind} {extra}"
                );
            }
        }
        for fields in [
            "kind='workspaces'\ncommand='true'",
            "kind='window-title'\ntext='override'",
            "kind='window-title'\nposition='middle'",
        ] {
            assert!(
                parse(&format!(
                    "version=1\n[panel]\nedge='top'\nheight=32\n[[panel.widgets]]\n{fields}"
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn rejects_unknown_widget_kinds_and_fields() {
        for fields in [
            "kind = 'exec'\ncommand = 'true'",
            "kind = 'clock'\ncommand = 'true'",
            "kind = 'text'\ntext = 'ok'\nunknown = true",
            "kind = 'clock'\nposition = 'middle'",
        ] {
            assert!(
                parse(&format!(
                    "version=1\n[panel]\nedge='top'\nheight=32\n[[panel.widgets]]\n{fields}"
                ))
                .is_err()
            );
        }
        assert!(
            parse("version=1\n[panel]\nedge='top'\nheight=32\n[panel.appearance]\nunknown=true")
                .is_err()
        );
    }

    #[test]
    fn validates_style_limits_and_color_without_panicking_on_unicode() {
        for field in [
            "font_size=0",
            "font_size=129",
            "padding=257",
            "gap=257",
            "font=''",
            "background='#zz0000'",
            "foreground='#ééé'",
            "background='red'",
        ] {
            assert!(
                parse(&format!(
                    "version=1\n[panel]\nedge='top'\nheight=32\n[panel.appearance]\n{field}"
                ))
                .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn bounds_widget_content_and_count() {
        let header = "version=1\n[panel]\nedge='top'\nheight=32\n";
        let widget = "[[panel.widgets]]\nkind='clock'\n";
        assert!(parse(&format!("{header}{}", widget.repeat(24))).is_ok());
        assert!(parse(&format!("{header}{}", widget.repeat(25))).is_err());
        let text = "x".repeat(1025);
        assert!(
            parse(&format!(
                "{header}[[panel.widgets]]\nkind='text'\ntext='{text}'"
            ))
            .is_err()
        );
        assert!(
            parse(&format!(
                "{header}[[panel.widgets]]\nkind='text'\ntext=\"a\\nb\""
            ))
            .is_err()
        );
    }
}

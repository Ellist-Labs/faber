pub mod default;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Raw RGBA color as 0xRRGGBBAA. Alpha FF = opaque.
pub type HexColor = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palette {
    pub crust: HexColor,
    pub mantle: HexColor,
    pub base: HexColor,
    pub surface0: HexColor,
    pub surface1: HexColor,
    pub surface2: HexColor,
    pub overlay0: HexColor,
    pub overlay1: HexColor,
    pub overlay2: HexColor,
    pub subtext0: HexColor,
    pub subtext1: HexColor,
    pub text: HexColor,
    pub lavender: HexColor,
    pub blue: HexColor,
    pub sapphire: HexColor,
    pub sky: HexColor,
    pub teal: HexColor,
    pub green: HexColor,
    pub yellow: HexColor,
    pub peach: HexColor,
    pub maroon: HexColor,
    pub red: HexColor,
    pub mauve: HexColor,
    pub pink: HexColor,
    pub flamingo: HexColor,
    pub rosewater: HexColor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticColors {
    // Surface
    pub bg: HexColor,
    pub bg_elevated: HexColor,
    pub bg_raised: HexColor,
    pub bg_overlay: HexColor,
    pub bg_sunken: HexColor,
    // Text
    pub text: HexColor,
    pub text_muted: HexColor,
    pub text_subtle: HexColor,
    pub text_on_accent: HexColor,
    pub text_disabled: HexColor,
    // Border
    pub border: HexColor,
    pub border_focus: HexColor,
    pub separator: HexColor,
    // Accent
    pub accent: HexColor,
    pub accent_hover: HexColor,
    pub accent_muted: HexColor,
    // Editor
    pub cursor: HexColor,
    pub selection: HexColor,
    pub word_highlight: HexColor,
    pub line_highlight: HexColor,
    pub gutter: HexColor,
    pub gutter_active: HexColor,
    pub match_bg: HexColor,
    pub match_active: HexColor,
    pub dirty: HexColor,
    // Status
    pub success: HexColor,
    pub warning: HexColor,
    pub error: HexColor,
    pub info: HexColor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightStyle {
    pub color: HexColor,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
}

impl HighlightStyle {
    pub const fn color(c: HexColor) -> Self {
        Self {
            color: c,
            bold: false,
            italic: false,
        }
    }

    pub const fn italic(c: HexColor) -> Self {
        Self {
            color: c,
            bold: false,
            italic: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntaxTheme {
    pub styles: Vec<HighlightStyle>,
    pub capture_name_map: BTreeMap<String, usize>,
}

impl SyntaxTheme {
    /// Build from ordered `(capture_name, style)` pairs.
    pub fn new(entries: impl IntoIterator<Item = (&'static str, HighlightStyle)>) -> Self {
        let mut styles = Vec::new();
        let mut capture_name_map = BTreeMap::new();
        for (name, style) in entries {
            let idx = styles.len();
            styles.push(style);
            capture_name_map.insert(name.to_string(), idx);
        }
        Self {
            styles,
            capture_name_map,
        }
    }

    /// Dotted-prefix fallback: "keyword.control.conditional" → "keyword.control" → "keyword".
    pub fn highlight_id(&self, capture_name: &str) -> Option<u32> {
        let first = capture_name.split('.').next().unwrap_or(capture_name);
        self.capture_name_map
            .range::<str, _>((
                std::ops::Bound::Included(first),
                std::ops::Bound::Included(capture_name),
            ))
            .rfind(|(key, _)| {
                *key == capture_name
                    || (capture_name.starts_with(*key)
                        && capture_name.as_bytes().get(key.len()).copied() == Some(b'.'))
            })
            .map(|(_, &idx)| idx as u32)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypographyRole {
    pub size_px: f32,
    pub weight: u16,
    pub line_height_px: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Typography {
    pub ui_family: String,
    pub mono_family: String,
    pub display: TypographyRole,
    pub heading: TypographyRole,
    pub body: TypographyRole,
    pub caption: TypographyRole,
    pub code: TypographyRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spacing {
    pub sp1: f32,
    pub sp2: f32,
    pub sp3: f32,
    pub sp4: f32,
    pub sp5: f32,
    pub sp6: f32,
    pub sp7: f32,
    pub sp8: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Radii {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialOpacity {
    /// Chrome surfaces (title bar, panels). 1.0 = opaque; reduce when blur is wired.
    pub chrome: f32,
    pub overlay: f32,
    pub scrim: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
    pub colors: SemanticColors,
    pub syntax: SyntaxTheme,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radii: Radii,
    pub material: MaterialOpacity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faber_dark_serializes_round_trip() {
        let theme = default::faber_dark();
        let json = serde_json::to_string(&theme).expect("serialize");
        let back: Theme = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, theme.name);
        assert_eq!(back.colors.bg, theme.colors.bg);
        assert!(back.syntax.highlight_id("keyword").is_some());
    }
}

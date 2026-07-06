pub mod default;

use serde::{Deserialize, Serialize};

/// Raw RGB color as 0xRRGGBB.
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
        Self { color: c, bold: false, italic: false }
    }

    pub const fn italic(c: HexColor) -> Self {
        Self { color: c, bold: false, italic: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntaxTheme {
    pub keyword: HighlightStyle,
    pub function: HighlightStyle,
    pub r#type: HighlightStyle,
    pub string: HighlightStyle,
    pub number: HighlightStyle,
    pub comment: HighlightStyle,
    pub constant: HighlightStyle,
    pub operator: HighlightStyle,
    pub punctuation: HighlightStyle,
    pub variable: HighlightStyle,
    pub property: HighlightStyle,
    pub attribute: HighlightStyle,
    pub namespace: HighlightStyle,
    pub tag: HighlightStyle,
    pub label: HighlightStyle,
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
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
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
    fn felix_dark_serializes_round_trip() {
        let theme = default::felix_dark();
        let json = serde_json::to_string(&theme).expect("serialize");
        let back: Theme = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, theme.name);
        assert_eq!(back.colors.bg, theme.colors.bg);
        assert_eq!(back.syntax.keyword.color, theme.syntax.keyword.color);
    }
}

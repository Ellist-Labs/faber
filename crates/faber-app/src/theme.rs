use std::sync::Arc;

use faber_lang::HighlightId;
use faber_theme::Theme as ThemeDef;
use gpui::{App, Global, Hsla, SharedString, rgba};

/// GPU-ready style for a single syntax highlight entry.
#[derive(Clone, Copy)]
pub struct SyntaxStyle {
    pub color: Hsla,
    pub italic: bool,
}

/// GPU-ready runtime theme. Cloned once per frame at the top of `render()`, then
/// passed by ref to helpers. Arc fields make the clone O(1).
#[allow(dead_code)] // some tokens unused until Wave 2 view adoption
#[derive(Clone)]
pub struct RuntimeTheme {
    // Surface
    pub bg: Hsla,
    pub bg_elevated: Hsla,
    pub bg_raised: Hsla,
    pub bg_overlay: Hsla,
    pub bg_sunken: Hsla,
    // Text
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_subtle: Hsla,
    pub text_on_accent: Hsla,
    pub text_disabled: Hsla,
    // Border
    pub border: Hsla,
    pub border_focus: Hsla,
    pub separator: Hsla,
    // Accent
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_muted: Hsla,
    // Editor
    pub cursor: Hsla,
    pub selection: Hsla,
    pub word_highlight: Hsla,
    pub line_highlight: Hsla,
    pub gutter: Hsla,
    pub gutter_active: Hsla,
    pub match_bg: Hsla,
    pub match_active: Hsla,
    pub dirty: Hsla,
    // Status
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub info: Hsla,
    // Syntax — data-driven via HighlightMap
    pub syntax_styles: Arc<[SyntaxStyle]>,
    syntax_theme: Arc<faber_theme::SyntaxTheme>,
    // Typography (pixel sizes; font family resolved by consumers)
    pub ui_family: SharedString,
    pub mono_family: SharedString,
    pub font_size_body: f32,
    pub font_size_caption: f32,
    pub font_size_code: f32,
    pub font_size_gutter: f32,
    pub font_size_heading: f32,
    pub line_height_code: f32,
    /// Advance width of one monospace cell — drives cursor x-position math.
    pub char_w_code: f32,
    // Spacing
    pub sp1: f32,
    pub sp2: f32,
    pub sp3: f32,
    pub sp4: f32,
    pub sp5: f32,
    pub sp6: f32,
    pub sp7: f32,
    pub sp8: f32,
    // Radii
    pub radius_xs: f32,
    pub radius_sm: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    pub radius_xl: f32,
    // Scrim / overlay opacity
    pub scrim: f32, // default 0.35
    // Layout sizes (px) — named for theming, avoids magic literals in views
    pub tab_h: f32,          // 36.0
    pub titlebar_h: f32,     // 38.0
    pub sidebar_w: f32,      // 240.0
    pub tree_row_h: f32,     // 25.0
    pub bottom_panel_h: f32, // 168.0
    pub status_bar_h: f32,   // 24.0
    pub right_panel_w: f32,  // 240.0
    pub panel_header_h: f32, // 30.0
}

impl Global for RuntimeTheme {}

fn h(hex: u32) -> Hsla {
    rgba(hex).into()
}

impl From<ThemeDef> for RuntimeTheme {
    fn from(t: ThemeDef) -> Self {
        Self::from_scaled(t, 1.0)
    }
}

impl RuntimeTheme {
    /// Build from a theme definition with all typography multiplied by
    /// `scale` (settings-driven; 1.0 = the theme's own sizes).
    pub fn from_scaled(t: ThemeDef, scale: f32) -> Self {
        let syntax_theme = Arc::new(t.syntax);
        let syntax_styles: Arc<[SyntaxStyle]> = syntax_theme
            .styles
            .iter()
            .map(|hs| SyntaxStyle {
                color: h(hs.color),
                italic: hs.italic,
            })
            .collect::<Vec<_>>()
            .into();
        let c = &t.colors;
        let ty = &t.typography;
        let sp = &t.spacing;
        let r = &t.radii;
        RuntimeTheme {
            bg: h(c.bg),
            bg_elevated: h(c.bg_elevated),
            bg_raised: h(c.bg_raised),
            bg_overlay: h(c.bg_overlay),
            bg_sunken: h(c.bg_sunken),
            text: h(c.text),
            text_muted: h(c.text_muted),
            text_subtle: h(c.text_subtle),
            text_on_accent: h(c.text_on_accent),
            text_disabled: h(c.text_disabled),
            border: h(c.border),
            border_focus: h(c.border_focus),
            separator: h(c.separator),
            accent: h(c.accent),
            accent_hover: h(c.accent_hover),
            accent_muted: h(c.accent_muted),
            cursor: h(c.cursor),
            selection: h(c.selection),
            word_highlight: h(c.word_highlight),
            line_highlight: h(c.line_highlight),
            gutter: h(c.gutter),
            gutter_active: h(c.gutter_active),
            match_bg: h(c.match_bg),
            match_active: h(c.match_active),
            dirty: h(c.dirty),
            success: h(c.success),
            warning: h(c.warning),
            error: h(c.error),
            info: h(c.info),
            syntax_styles,
            syntax_theme,
            ui_family: ty.ui_family.clone().into(),
            mono_family: ty.mono_family.clone().into(),
            font_size_body: ty.body.size_px * scale,
            font_size_caption: ty.caption.size_px * scale,
            font_size_code: ty.code.size_px * scale,
            font_size_gutter: ty.code.size_px * scale * 0.85,
            font_size_heading: ty.heading.size_px * scale,
            line_height_code: ty.code.line_height_px * scale,
            // Linear fallback from the measured 8.4px @ 13px Monaco cell;
            // apply_settings overwrites with a real text-system measurement.
            char_w_code: 8.4 * (ty.code.size_px * scale) / 13.0,
            sp1: sp.sp1,
            sp2: sp.sp2,
            sp3: sp.sp3,
            sp4: sp.sp4,
            sp5: sp.sp5,
            sp6: sp.sp6,
            sp7: sp.sp7,
            sp8: sp.sp8,
            radius_xs: r.xs,
            radius_sm: r.sm,
            radius_md: r.md,
            radius_lg: r.lg,
            radius_xl: r.xl,
            scrim: t.material.scrim,
            tab_h: 36.0,
            titlebar_h: 38.0,
            sidebar_w: 240.0,
            tree_row_h: 25.0,
            bottom_panel_h: 168.0,
            status_bar_h: 24.0,
            right_panel_w: 240.0,
            panel_header_h: 30.0,
        }
    }

    /// Get the style for a highlight id (None if id is out of range).
    pub fn syntax_style(&self, id: HighlightId) -> Option<SyntaxStyle> {
        self.syntax_styles.get(id.index()).copied()
    }

    /// Resolve a capture name to its HighlightId (dotted-name fallback).
    pub fn highlight_id(&self, capture_name: &str) -> Option<HighlightId> {
        self.syntax_theme
            .highlight_id(capture_name)
            .map(|idx| HighlightId::from_style_index(idx as usize))
    }

    /// Convenience: color for a highlight id, falling back to `self.text`.
    pub fn syntax_color(&self, id: HighlightId) -> Hsla {
        self.syntax_style(id).map(|s| s.color).unwrap_or(self.text)
    }
}

/// Rebuild the theme global from current settings and repaint every window.
/// Called at startup and after each settings change.
pub fn apply_settings(cx: &mut App) {
    let settings = cx.global::<crate::settings_view::SettingsStore>().0.clone();
    let scale = settings.font_size / faber_settings::DEFAULT_FONT_SIZE;
    let mut rt = RuntimeTheme::from_scaled(faber_theme::default::faber_dark(), scale);
    let font_id = cx
        .text_system()
        .resolve_font(&gpui::font(rt.mono_family.clone()));
    if let Ok(em) = cx
        .text_system()
        .em_advance(font_id, gpui::px(rt.font_size_code))
    {
        rt.char_w_code = f32::from(em);
    }
    cx.set_global(rt);
    cx.refresh_windows();
}

/// Extension trait — gives every `AppContext`-carrying type `cx.theme()`.
pub trait ActiveTheme {
    fn theme(&self) -> &RuntimeTheme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &RuntimeTheme {
        self.global::<RuntimeTheme>()
    }
}

impl<T: 'static> ActiveTheme for gpui::Context<'_, T> {
    fn theme(&self) -> &RuntimeTheme {
        self.global::<RuntimeTheme>()
    }
}

use crate::{
    HighlightStyle, MaterialOpacity, Palette, Radii, SemanticColors, Spacing, SyntaxTheme, Theme,
    Typography, TypographyRole,
};

/// Built-in dark theme based on Catppuccin Mocha.
pub fn faber_dark() -> Theme {
    Theme {
        name: String::from("Faber Dark"),
        palette: palette(),
        colors: semantic(),
        syntax: syntax(),
        typography: typography(),
        spacing: spacing(),
        radii: radii(),
        material: material(),
    }
}

fn palette() -> Palette {
    Palette {
        crust: 0x11111b,
        mantle: 0x181825,
        base: 0x1e1e2e,
        surface0: 0x313244,
        surface1: 0x45475a,
        surface2: 0x585b70,
        overlay0: 0x6c7086,
        overlay1: 0x7f849c,
        overlay2: 0x9399b2,
        subtext0: 0xa6adc8,
        subtext1: 0xbac2de,
        text: 0xcdd6f4,
        lavender: 0xb4befe,
        blue: 0x89b4fa,
        sapphire: 0x74c7ec,
        sky: 0x89dceb,
        teal: 0x94e2d5,
        green: 0xa6e3a1,
        yellow: 0xf9e2af,
        peach: 0xfab387,
        maroon: 0xeba0ac,
        red: 0xf38ba8,
        mauve: 0xcba6f7,
        pink: 0xf5c2e7,
        flamingo: 0xf2cdcd,
        rosewater: 0xf5e0dc,
    }
}

fn semantic() -> SemanticColors {
    SemanticColors {
        // Surface hierarchy
        bg: 0x1e1e2e,         // base
        bg_elevated: 0x181825, // mantle (chrome, title bar)
        bg_overlay: 0x313244,  // surface0 (popovers, dropdowns)
        bg_sunken: 0x11111b,   // crust (inset areas)
        // Text
        text: 0xcdd6f4,          // text
        text_muted: 0xbac2de,    // subtext1
        text_subtle: 0xa6adc8,   // subtext0
        text_on_accent: 0x1e1e2e, // base (text on mauve accent)
        text_disabled: 0x6c7086,  // overlay0
        // Border
        border: 0x45475a,       // surface1
        border_focus: 0xcba6f7, // mauve
        separator: 0x313244,    // surface0
        // Accent (mauve = Catppuccin signature)
        accent: 0xcba6f7,       // mauve
        accent_hover: 0xb4befe, // lavender (lighter on hover)
        accent_muted: 0x45475a, // surface1 (subtle accent bg)
        // Editor
        cursor: 0xcdd6f4,          // text (bar cursor)
        selection: 0x45475a,       // surface1
        line_highlight: 0x313244,  // surface0
        gutter: 0x6c7086,          // overlay0
        gutter_active: 0xcdd6f4,   // text
        match_bg: 0x4a4f6a,        // mid-tone blue-grey (search match)
        match_active: 0x7f849c,    // overlay1 (active match)
        dirty: 0xf38ba8,           // red (unsaved indicator)
        // Status
        success: 0xa6e3a1, // green
        warning: 0xf9e2af, // yellow
        error: 0xf38ba8,   // red
        info: 0x89b4fa,    // blue
    }
}

fn syntax() -> SyntaxTheme {
    SyntaxTheme {
        keyword: HighlightStyle::italic(0xcba6f7),  // mauve, italic
        function: HighlightStyle::color(0x89b4fa),  // blue
        r#type: HighlightStyle::color(0xf9e2af),    // yellow
        string: HighlightStyle::color(0xa6e3a1),    // green
        number: HighlightStyle::color(0xfab387),    // peach
        comment: HighlightStyle::italic(0x6c7086),  // overlay0, italic
        constant: HighlightStyle::color(0xfab387),  // peach
        operator: HighlightStyle::color(0x89dceb),  // sky
        punctuation: HighlightStyle::color(0xcdd6f4), // text (neutral)
        variable: HighlightStyle::color(0xcdd6f4),  // text
        property: HighlightStyle::color(0x89dceb),  // sky
        attribute: HighlightStyle::color(0xf5c2e7), // pink
        namespace: HighlightStyle::color(0xf9e2af), // yellow
        tag: HighlightStyle::color(0xcba6f7),       // mauve
        label: HighlightStyle::color(0x89dceb),     // sky
    }
}

fn typography() -> Typography {
    Typography {
        ui_family: String::from(".SystemUIFont"),
        mono_family: String::from("Menlo"),
        display: TypographyRole { size_px: 20.0, weight: 600, line_height_px: 28.0 },
        heading: TypographyRole { size_px: 14.0, weight: 600, line_height_px: 20.0 },
        body: TypographyRole { size_px: 13.0, weight: 400, line_height_px: 18.0 },
        caption: TypographyRole { size_px: 11.0, weight: 400, line_height_px: 16.0 },
        code: TypographyRole { size_px: 13.0, weight: 400, line_height_px: 20.0 },
    }
}

fn spacing() -> Spacing {
    Spacing {
        sp1: 2.0,
        sp2: 4.0,
        sp3: 6.0,
        sp4: 8.0,
        sp5: 12.0,
        sp6: 16.0,
        sp7: 24.0,
        sp8: 32.0,
    }
}

fn radii() -> Radii {
    Radii {
        sm: 4.0,
        md: 6.0,
        lg: 8.0,
    }
}

fn material() -> MaterialOpacity {
    MaterialOpacity {
        chrome: 1.0,  // opaque now; reduce to ~0.85 when blur lands
        overlay: 1.0,
        scrim: 0.5,
    }
}

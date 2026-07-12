use crate::{
    HighlightStyle, MaterialOpacity, Palette, Radii, SemanticColors, Spacing, SyntaxTheme, Theme,
    Typography, TypographyRole,
};

/// Built-in dark theme — "Focused Glass" OLED Black (docs/ui-design-spec.md §2).
/// All colors are 0xRRGGBBAA.
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

/// Legacy palette slots (inert — semantic tokens below are authoritative).
fn palette() -> Palette {
    Palette {
        crust: 0x080808FF,
        mantle: 0x0D0D0DFF,
        base: 0x000000FF,
        surface0: 0x1A1A1AFF,
        surface1: 0x262626FF,
        surface2: 0x333333FF,
        overlay0: 0x555555FF,
        overlay1: 0x888888FF,
        overlay2: 0xAAAAAAFF,
        subtext0: 0x888888FF,
        subtext1: 0xBBBBBBFF,
        text: 0xFFFFFFFF,
        lavender: 0x7472E8FF,
        blue: 0x82AAFFFF,
        sapphire: 0x89DDFFFF,
        sky: 0x89DDFFFF,
        teal: 0xB2CCD6FF,
        green: 0xC3E88DFF,
        yellow: 0xFFCB6BFF,
        peach: 0xF78C6CFF,
        maroon: 0xFF453AFF,
        red: 0xFF453AFF,
        mauve: 0xCF8EF4FF,
        pink: 0xCF8EF4FF,
        flamingo: 0xFF9F0AFF,
        rosewater: 0xFF9F0AFF,
    }
}

fn semantic() -> SemanticColors {
    SemanticColors {
        // Surface hierarchy (§2.1)
        bg: 0x000000FF,          // window background, active tab
        bg_elevated: 0x0D0D0DFF, // title bar, tab bar, bottom panel, status bar
        bg_raised: 0x1A1A1AFF,   // hover rows, badges, item icon fills
        bg_overlay: 0x000000C2,  // glass surface base (§3)
        bg_sunken: 0x080808FF,   // gutter background
        // Text (§2.2)
        text: 0xFFFFFFFF,
        text_muted: 0x9A9A9AFF,  // #9A9A9A — contrast 5.5:1 on black (WCAG AA)
        text_subtle: 0x6E6E6EFF, // #6E6E6E — more visible than previous #555
        text_on_accent: 0xFFFFFFFF,
        text_disabled: 0x555555FF,
        // Border (§2.3)
        border: 0xFFFFFF12,       // white 7%
        border_focus: 0xFFFFFF1F, // white 12%
        separator: 0xFFFFFF12,
        // Accent (§2.4)
        accent: 0x5E5CE6FF,
        accent_hover: 0x7472E8FF,
        accent_muted: 0x5E5CE62E, // 18%
        // Editor (§2.5)
        cursor: 0x5E5CE6FF,
        selection: 0x5E5CE666,      // 40% — enough contrast on OLED black
        word_highlight: 0x5E5CE64D, // 30%
        line_highlight: 0xFFFFFF0D, // white 5%
        // gutter tokens keep their text-color semantic (line numbers);
        // the gutter *background* maps to bg_sunken.
        gutter: 0x6E6E6EFF,
        gutter_active: 0x9A9A9AFF,
        match_bg: 0x5E5CE666,     // 40%
        match_active: 0x5E5CE699, // 60%
        dirty: 0x5E5CE6FF,
        // Status (§2.6)
        success: 0x30D158FF,
        warning: 0xFF9F0AFF,
        error: 0xFF453AFF,
        info: 0x5E5CE6FF,
    }
}

fn syntax() -> SyntaxTheme {
    SyntaxTheme {
        keyword: HighlightStyle::italic(0xCF8EF4FF),
        function: HighlightStyle::color(0x82AAFFFF),
        r#type: HighlightStyle::color(0xFFCB6BFF),
        string: HighlightStyle::color(0xC3E88DFF),
        number: HighlightStyle::color(0xF78C6CFF),
        comment: HighlightStyle::italic(0x546E7AFF),
        constant: HighlightStyle::color(0xF78C6CFF),
        operator: HighlightStyle::color(0x89DDFFFF),
        punctuation: HighlightStyle::color(0xFFFFFF99), // white 60% — visible on OLED black
        variable: HighlightStyle::color(0xFFFFFFFF),
        property: HighlightStyle::color(0xB2CCD6FF),
        attribute: HighlightStyle::color(0xFF9F0AFF),
        namespace: HighlightStyle::color(0xFFCB6BFF),
        tag: HighlightStyle::color(0xCF8EF4FF),
        label: HighlightStyle::color(0x89DDFFFF),
    }
}

fn typography() -> Typography {
    Typography {
        ui_family: String::from(".SystemUIFont"),
        mono_family: String::from("Menlo"),
        display: TypographyRole {
            size_px: 17.0,
            weight: 700,
            line_height_px: 20.0,
        },
        heading: TypographyRole {
            size_px: 13.0,
            weight: 600,
            line_height_px: 18.0,
        },
        body: TypographyRole {
            size_px: 13.0,
            weight: 400,
            line_height_px: 19.0,
        },
        caption: TypographyRole {
            size_px: 12.0,
            weight: 400,
            line_height_px: 18.0,
        },
        code: TypographyRole {
            size_px: 13.0,
            weight: 400,
            line_height_px: 21.0,
        },
    }
}

fn spacing() -> Spacing {
    Spacing {
        sp1: 2.0,
        sp2: 4.0,
        sp3: 6.0,
        sp4: 8.0,
        sp5: 10.0,
        sp6: 12.0,
        sp7: 16.0,
        sp8: 20.0,
    }
}

fn radii() -> Radii {
    Radii {
        xs: 4.0,
        sm: 6.0,
        md: 8.0,
        lg: 10.0,
        xl: 14.0,
    }
}

fn material() -> MaterialOpacity {
    MaterialOpacity {
        chrome: 1.0,
        overlay: 1.0,
        scrim: 0.35,
    }
}

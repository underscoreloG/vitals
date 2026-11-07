//! kvlh.org brand, values from kvlh-brand/tokens.css. Red is for the brand mark only.

use eframe::egui::{self, Color32, Theme, Visuals};

/// Rendered from assets/vitals-icon.svg by assets/make-icon.ps1.
pub const ICON: &[u8] = include_bytes!("../assets/vitals-64.png");

/// --accent
pub const ACCENT: Color32 = Color32::from_rgb(0xE1, 0x0E, 0x26);

pub struct Tokens {
    pub bg: Color32,
    pub elevated: Color32,
    pub fg: Color32,
    pub muted: Color32,
    pub border: Color32,
    pub info: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
}

const DARK: Tokens = Tokens {
    bg: Color32::from_rgb(0x14, 0x14, 0x1A),
    elevated: Color32::from_rgb(0x1B, 0x1B, 0x22),
    fg: Color32::from_rgb(0xF4, 0xF4, 0xF7),
    muted: Color32::from_rgb(0x9A, 0x9A, 0xA8),
    border: Color32::from_rgb(0x23, 0x23, 0x2C),
    info: Color32::from_rgb(0x60, 0xA5, 0xFA),
    success: Color32::from_rgb(0x4A, 0xDE, 0x80),
    warning: Color32::from_rgb(0xFB, 0xBF, 0x24),
    danger: Color32::from_rgb(0xFF, 0x8A, 0x8A),
};

const LIGHT: Tokens = Tokens {
    bg: Color32::from_rgb(0xF7, 0xF7, 0xF9),
    elevated: Color32::WHITE,
    fg: Color32::from_rgb(0x14, 0x14, 0x1A),
    muted: Color32::from_rgb(0x5C, 0x5C, 0x6B),
    border: Color32::from_rgb(0xDC, 0xDC, 0xE3),
    info: Color32::from_rgb(0x1D, 0x4E, 0xD8),
    success: Color32::from_rgb(0x15, 0x80, 0x3D),
    warning: Color32::from_rgb(0xB4, 0x53, 0x09),
    danger: Color32::from_rgb(0xC1, 0x00, 0x1B),
};

pub fn tokens(dark: bool) -> &'static Tokens {
    if dark { &DARK } else { &LIGHT }
}

/// Follows the OS light or dark setting.
pub fn apply(ctx: &egui::Context) {
    for (theme, mut v, t) in [(Theme::Dark, Visuals::dark(), &DARK), (Theme::Light, Visuals::light(), &LIGHT)] {
        v.panel_fill = t.bg;
        v.override_text_color = Some(t.fg);
        v.weak_text_color = Some(t.muted);
        ctx.set_visuals_of(theme, v);
    }
}

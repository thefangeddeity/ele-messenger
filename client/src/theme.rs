//! The suite's look: the same named colours as the camera viewer and the
//! server's web client, monospace throughout, dark unless switched to light.

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub struct Tokens {
    pub bg: Color32,
    pub panel: Color32,
    pub panel_2: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub danger: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Tokens = Tokens {
    bg: hex(0x111113),
    panel: hex(0x1c1c1e),
    panel_2: hex(0x242426),
    border: hex(0x38383a),
    border_strong: hex(0x48484a),
    text: hex(0xf5f5f7),
    text_dim: hex(0x98989d),
    text_muted: hex(0x6e6e73),
    accent: hex(0x0a84ff),
    success: hex(0x30d158),
    danger: hex(0xff453a),
};

pub const LIGHT: Tokens = Tokens {
    bg: hex(0xe7e6df),
    panel: hex(0xfcfbf7),
    panel_2: hex(0xf0efe9),
    border: hex(0xdcdad0),
    border_strong: hex(0xcbc9bd),
    text: hex(0x073642),
    text_dim: hex(0x586e75),
    text_muted: hex(0x93a1a1),
    accent: hex(0x0a62c9),
    success: hex(0x1b7a46),
    // The web tokens leave danger unchanged in light mode.
    danger: hex(0xff453a),
};

pub fn tokens(light: bool) -> &'static Tokens {
    if light { &LIGHT } else { &DARK }
}

pub fn apply(ctx: &egui::Context, light: bool) {
    let t = tokens(light);
    let mut v = if light { Visuals::light() } else { Visuals::dark() };
    v.override_text_color = Some(t.text);
    v.weak_text_color = Some(t.text_dim);
    v.panel_fill = t.bg;
    v.window_fill = t.panel;
    v.extreme_bg_color = t.panel_2;
    v.faint_bg_color = t.panel;
    v.hyperlink_color = t.accent;
    v.error_fg_color = t.danger;
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, t.text);
    v.window_stroke = Stroke::new(1.0, t.border);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_stroke = Stroke::new(1.0, t.border);
        w.fg_stroke = Stroke::new(1.0, t.text);
        w.corner_radius = 6.into();
    }
    v.widgets.noninteractive.bg_fill = t.panel;
    v.widgets.noninteractive.weak_bg_fill = t.panel;
    v.widgets.inactive.bg_fill = t.panel_2;
    v.widgets.inactive.weak_bg_fill = t.panel_2;
    v.widgets.hovered.bg_fill = t.panel_2;
    v.widgets.hovered.weak_bg_fill = t.panel_2;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, t.border_strong);
    v.widgets.active.bg_fill = t.accent;
    v.widgets.active.weak_bg_fill = t.accent;
    // eframe follows the system theme unless told otherwise, and would put
    // its own visuals back; pin ours to the chosen theme.
    let theme = if light { egui::Theme::Light } else { egui::Theme::Dark };
    ctx.set_visuals_of(theme, v);
    ctx.set_theme(theme);

    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(17.0, FontFamily::Monospace)),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Monospace)),
            (TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(13.5, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Monospace)),
        ]
        .into();
    });
}

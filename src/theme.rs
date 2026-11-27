use crate::i18n::tr;
use gpui::{Rgba, rgb, rgba};

use crate::platform;
use crate::settings::{Accent, Settings, ThemeMode};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Rgba,
    pub panel: Rgba,
    pub panel_soft: Rgba,
    pub sidebar: Rgba,
    pub card: Rgba,
    pub card_hover: Rgba,
    pub border: Rgba,
    pub border_soft: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub dim: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_soft: Rgba,
    pub accent_faint: Rgba,
    pub accent_fg: Rgba,
    pub danger: Rgba,
    pub danger_soft: Rgba,
    pub success: Rgba,
    pub selection: Rgba,
    pub overlay: Rgba,
    pub input: Rgba,
    pub titlebar: Rgba,
    pub toolbar: Rgba,
}

pub fn is_dark(settings: &Settings) -> bool {
    match settings.theme {
        ThemeMode::Dark | ThemeMode::MidnightSlate | ThemeMode::AyuMirage | ThemeMode::Cherry => true,
        ThemeMode::Light => false,
        ThemeMode::System => !platform::system_is_light(),
    }
}

pub fn palette(settings: &Settings) -> Palette {
    let dark = is_dark(settings);
    let accent = settings.accent.rgb();
    let accent_color = rgb(accent);
    let accent_soft = rgba((accent << 8) | 0x2a);
    let accent_faint = rgba((accent << 8) | 0x12);

    if dark {
        // Paleta oscura tomada de la captura de referencia: superficies
        // #1e1f22, barra de título y de estado #18191c, bordes #2f3135 y
        // texto #dcdde0. Los controles elevados usan #232428 y la selección
        // conserva el tinte cálido del acento.
        let mut palette = Palette {
            bg: rgb(0x1e1f22),
            panel: rgb(0x1e1f22),
            panel_soft: rgb(0x232428),
            sidebar: rgb(0x1e1f22),
            card: rgb(0x232428),
            card_hover: rgb(0x2a2c30),
            border: rgb(0x2f3135),
            border_soft: rgb(0x26282c),
            text: rgb(0xdcdde0),
            muted: rgb(0xb0b1b4),
            dim: rgb(0x747578),
            accent: accent_color,
            accent_hover: rgb(lighten(accent, 0.12)),
            accent_soft,
            accent_faint,
            accent_fg: rgb(0x18191c),
            danger: rgb(0xff6b6b),
            danger_soft: rgba(0xff6b6b22),
            success: rgb(0x63d6a3),
            selection: rgba((accent << 8) | 0x24),
            overlay: rgba(0x0f0f0fcc),
            input: rgb(0x1e1f22),
            titlebar: rgb(0x18191c),
            toolbar: rgb(0x1e1f22),
        };
        let surfaces = match settings.theme {
            ThemeMode::MidnightSlate => Some([
                0x181d23, 0x1c222a, 0x151a1f, 0x15191e, 0x232c36, 0x33404d, 0x2a3540,
                0xe3eaf3, 0xb3c2d3, 0x8295aa,
            ]),
            ThemeMode::AyuMirage => Some([
                0x1f2430, 0x252a36, 0x222733, 0x1c212c, 0x303642, 0x393e4a, 0x303541,
                0xcccac2, 0xa6afbf, 0x8a98ad,
            ]),
            ThemeMode::Cherry => Some([
                0x2c222c, 0x342732, 0x241b24, 0x211820, 0x443144, 0x654c5f, 0x473543,
                0xf1e5ed, 0xd0b6c9, 0xad91a6,
            ]),
            _ => None,
        };
        if let Some([bg, card, sidebar, title, hover, border, soft, text, muted, dim]) = surfaces {
            palette.bg = rgb(bg);
            palette.panel = rgb(bg);
            palette.panel_soft = rgb(card);
            palette.sidebar = rgb(sidebar);
            palette.card = rgb(card);
            palette.card_hover = rgb(hover);
            palette.border = rgb(border);
            palette.border_soft = rgb(soft);
            palette.text = rgb(text);
            palette.muted = rgb(muted);
            palette.dim = rgb(dim);
            palette.input = rgb(bg);
            palette.titlebar = rgb(title);
            palette.toolbar = rgb(bg);
        }
        if settings.theme == ThemeMode::MidnightSlate {
            palette.selection = rgba((accent << 8) | 0x43);
            palette.overlay = rgba(0x10151ab8);
        }
        if settings.theme == ThemeMode::AyuMirage {
            palette.selection = rgb(0x343e50);
            palette.overlay = rgba(0x141925ba);
            palette.danger = rgb(0xf28779);
            palette.success = rgb(0xd5ff80);
        }
        palette
    } else {
        Palette {
            bg: rgb(0xf3f3f5),
            panel: rgb(0xffffff),
            panel_soft: rgb(0xf7f7f9),
            sidebar: rgb(0xf5f5f7),
            card: rgb(0xffffff),
            card_hover: rgb(0xf0f0f3),
            border: rgb(0xdedee4),
            border_soft: rgb(0xebebef),
            text: rgb(0x1c1c21),
            muted: rgb(0x64646f),
            dim: rgb(0x9696a1),
            accent: accent_color,
            accent_hover: rgb(darken(accent, 0.08)),
            accent_soft,
            accent_faint,
            accent_fg: rgb(0x20160a),
            danger: rgb(0xd93b3b),
            danger_soft: rgba(0xd93b3b1c),
            success: rgb(0x1c9b68),
            selection: rgba((accent << 8) | 0x26),
            overlay: rgba(0x101014cc),
            input: rgb(0xf7f7f9),
            titlebar: rgb(0xfbfbfc),
            toolbar: rgb(0xffffff),
        }
    }
}

/// Aplica la paleta de Xool Files a los componentes de `gpui-component` (campos de
/// texto, cursor, selección y menús) para que no se queden con los colores
/// oscuros genéricos de la librería.
pub fn apply_component_theme(settings: &Settings, cx: &mut gpui::App) {
    let p = palette(settings);
    let theme = gpui_component::Theme::global_mut(cx);
    let c = &mut theme.colors;
    c.background = p.bg.into();
    c.foreground = p.text.into();
    c.border = p.border.into();
    c.input = p.input.into();
    c.caret = p.accent.into();
    c.selection = p.selection.into();
    c.muted = p.panel_soft.into();
    c.muted_foreground = p.dim.into();
    c.accent = p.accent.into();
    c.accent_foreground = p.accent_fg.into();
    c.primary = p.accent.into();
    c.primary_foreground = p.accent_fg.into();
    c.primary_hover = p.accent_hover.into();
    c.primary_active = p.accent_hover.into();
    c.secondary = p.card.into();
    c.secondary_foreground = p.text.into();
    c.secondary_hover = p.card_hover.into();
    c.popover = p.panel.into();
    c.popover_foreground = p.text.into();
    c.title_bar = p.titlebar.into();
    c.title_bar_border = p.border.into();
    c.sidebar = p.sidebar.into();
    c.sidebar_foreground = p.muted.into();
    c.sidebar_border = p.border_soft.into();
    c.sidebar_accent = p.accent.into();
    c.sidebar_accent_foreground = p.text.into();
    c.list = p.panel.into();
    c.list_active = p.selection.into();
    c.list_hover = p.card_hover.into();
    c.scrollbar = p.bg.into();
    c.scrollbar_thumb = p.border.into();
    c.scrollbar_thumb_hover = p.dim.into();
    c.overlay = p.overlay.into();
    c.ring = p.accent.into();
}

pub fn accents() -> [(Accent, &'static str); 8] {
    [
    (Accent::Blue, tr("Azul", "Blue")), (Accent::Red, tr("Rojo", "Red")), (Accent::Rose, tr("Rosado", "Pink")),
    (Accent::Yellow, tr("Amarillo", "Yellow")), (Accent::Amber, tr("Naranja", "Orange")), (Accent::Green, tr("Verde", "Green")),
    (Accent::Purple, tr("Morado", "Purple")), (Accent::Gray, tr("Gris", "Gray")),
    ]
}

fn lighten(hex: u32, amount: f32) -> u32 {
    let r = ((hex >> 16) & 0xff) as f32;
    let g = ((hex >> 8) & 0xff) as f32;
    let b = (hex & 0xff) as f32;
    let mix = |v: f32| (v + (255.0 - v) * amount).round() as u32;
    (mix(r) << 16) | (mix(g) << 8) | mix(b)
}

fn darken(hex: u32, amount: f32) -> u32 {
    let r = ((hex >> 16) & 0xff) as f32;
    let g = ((hex >> 8) & 0xff) as f32;
    let b = (hex & 0xff) as f32;
    let mix = |v: f32| (v * (1.0 - amount)).round() as u32;
    (mix(r) << 16) | (mix(g) << 8) | mix(b)
}

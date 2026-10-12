//! Оформление: цвета, шрифт, палитры графиков.
//!
//! Образ — измерительный прибор: сине-графитовые панели, тёмное поле графика,
//! янтарная подсветка выбранного. Палитра серий проверена на различимость
//! (в том числе при дальтонизме) на поле `SURFACE` валидатором навыка dataviz.

use std::sync::Arc;

use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle};

/// Поле графика.
pub const SURFACE: Color32 = Color32::from_rgb(0x13, 0x19, 0x20);
/// Панели.
pub const PANEL: Color32 = Color32::from_rgb(0x1B, 0x22, 0x2C);
/// Кнопки и поля ввода.
const RAISED: Color32 = Color32::from_rgb(0x27, 0x32, 0x42);
const RAISED_HOVER: Color32 = Color32::from_rgb(0x33, 0x41, 0x55);
/// Линии сетки и разделители.
const HAIRLINE: Color32 = Color32::from_rgb(0x2E, 0x3A, 0x4B);
/// Основной текст.
pub const INK: Color32 = Color32::from_rgb(0xE4, 0xE9, 0xF0);
/// Второстепенный текст.
pub const MUTED: Color32 = Color32::from_rgb(0x93, 0xA0, 0xB3);
/// Предупреждения.
pub const WARN: Color32 = Color32::from_rgb(0xE6, 0x67, 0x67);
/// Янтарная подсветка выбранного.
pub const ACCENT: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x29);

/// Цвета серий в фиксированном порядке (синий, оранжевый, бирюзовый, жёлтый,
/// пурпурный, зелёный, фиолетовый, красный).
const SERIES: [Color32; 8] = [
    Color32::from_rgb(0x39, 0x87, 0xE5),
    Color32::from_rgb(0xD9, 0x59, 0x26),
    Color32::from_rgb(0x19, 0x9E, 0x70),
    Color32::from_rgb(0xC9, 0x85, 0x00),
    Color32::from_rgb(0xD5, 0x51, 0x81),
    Color32::from_rgb(0x00, 0x83, 0x00),
    Color32::from_rgb(0x90, 0x85, 0xE9),
    Color32::from_rgb(0xE6, 0x67, 0x67),
];

/// Цвет закреплён за сигналом, а не за его местом в списке: гироскоп всегда
/// синий, задание — оранжевое, на какой бы вкладке и оси они ни были.
pub fn series_color(name: &str) -> Color32 {
    let (base, index) = match name.split_once('[') {
        Some((base, rest)) => (base, rest.trim_end_matches(']').parse().unwrap_or(0)),
        None => (name, 0),
    };
    match base {
        "gyroADC" => SERIES[0],
        "setpoint" => SERIES[1],
        "gyroUnfilt" => SERIES[2],
        "axisP" => SERIES[3],
        "axisI" => SERIES[4],
        "axisD" => SERIES[7],
        "axisF" => SERIES[6],
        "axisSum" => SERIES[5],
        "rcCommand" => MUTED,
        "ошибка" => SERIES[5],
        "пробный фильтр" => SERIES[6],
        _ => SERIES[index % SERIES.len()],
    }
}

/// Цвет группы с номером `index` там, где линии различаются не сигналом, а группой
/// (отклик по скорости стиков или по газу): синий, оранжевый, бирюзовый — первые три
/// цвета палитры различимы в любых парах.
pub fn nth_series(index: usize) -> Color32 {
    SERIES[index % SERIES.len()]
}

/// Палитра уровня для карт (inferno): тёмное — тихо, светлое — громко.
pub fn colormap(t: f32) -> Color32 {
    const STOPS: [[f32; 3]; 8] = [
        [0.0, 0.0, 4.0],
        [40.0, 11.0, 84.0],
        [101.0, 21.0, 110.0],
        [159.0, 42.0, 99.0],
        [212.0, 72.0, 66.0],
        [245.0, 125.0, 21.0],
        [250.0, 193.0, 39.0],
        [252.0, 255.0, 164.0],
    ];
    let pos = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let i = (pos as usize).min(STOPS.len() - 2);
    let mix = |c: usize| (STOPS[i][c] + (STOPS[i + 1][c] - STOPS[i][c]) * (pos - i as f32)) as u8;
    Color32::from_rgb(mix(0), mix(1), mix(2))
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("plex".into(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/IBMPlexSans-Regular.ttf"))));
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "plex".into());
    ctx.set_fonts(fonts);

    // Оформление рассчитано на тёмную тему и не должно зависеть от темы системы.
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.global_style()).clone();
    for (text_style, size) in [(TextStyle::Small, 12.0), (TextStyle::Body, 14.0), (TextStyle::Button, 14.0), (TextStyle::Heading, 18.0)] {
        style.text_styles.insert(text_style, FontId::proportional(size));
    }
    style.spacing.item_spacing = egui::vec2(10.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = SURFACE;
    v.faint_bg_color = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.noninteractive.fg_stroke.color = INK;
    for (widget, fill) in [(&mut v.widgets.inactive, RAISED), (&mut v.widgets.hovered, RAISED_HOVER), (&mut v.widgets.active, RAISED_HOVER)] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke.color = INK;
    }
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, SURFACE);
    ctx.set_global_style(style);
}

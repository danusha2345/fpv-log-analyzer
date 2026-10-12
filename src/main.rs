// В готовой сборке под Windows не открывать окно консоли рядом с программой.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bbl;
mod dsp;
mod tabs;
mod theme;

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_plot::{AxisHints, Line, LineStyle, Plot, PlotImage, PlotPoint, PlotPoints, Text, VLine};

const AXES: [&str; 3] = ["Roll", "Pitch", "Yaw"];
/// Поля с компонентами по осям дрона: `имя[0..2]` = Roll, Pitch, Yaw.
const AXIS_BASES: [&str; 10] = ["gyroADC", "gyroUnfilt", "setpoint", TRACKING_ERROR, "rcCommand", "axisP", "axisI", "axisD", "axisF", "axisSum"];
/// Расчётное поле: задание минус гироскоп.
const TRACKING_ERROR: &str = "ошибка";
/// Что включено на кривых сразу после загрузки.
const DEFAULT_CURVES: [&str; 2] = ["gyroADC", "setpoint"];
/// Сигналы, для которых строится спектр.
const SPECTRUM_BASES: [&str; 3] = ["gyroUnfilt", "gyroADC", "axisD"];
/// Ширина поля под подписи оси Y — одинаковая, чтобы графики друг под другом совпадали по X.
const Y_AXIS_WIDTH: f32 = 56.0;
const TRIAL_FILTER: &str = "пробный фильтр";

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Overview,
    Curves,
    Spectrum,
    Heatmap,
    Spectrogram,
    Step,
    Error,
    Motors,
    Settings,
}

/// Вкладки по порядку: `(вкладка, название, имя для командной строки и файлов снимков)`.
const TABS: [(Tab, &str, &str); 9] = [
    (Tab::Overview, "Обзор", "overview"),
    (Tab::Curves, "Кривые", "curves"),
    (Tab::Spectrum, "Спектр", "spectrum"),
    (Tab::Heatmap, "Карта шума", "heatmap"),
    (Tab::Spectrogram, "Спектрограмма", "spectrogram"),
    (Tab::Step, "Отклик", "step"),
    (Tab::Error, "Ошибка", "error"),
    (Tab::Motors, "Моторы", "motors"),
    (Tab::Settings, "Настройки", "settings"),
];

/// Поле с компонентами по осям; у некоторых (axisD) оси Yaw может не быть.
struct AxisField {
    base: &'static str,
    cols: [Option<usize>; 3],
    on: bool,
}

type Spectra = Vec<(&'static str, [Vec<[f64; 2]>; 3])>;

/// Вид карты шума.
#[derive(Clone, Copy)]
enum MapKind {
    Throttle = 0,
    Time = 1,
    Rpm = 2,
}

/// Матрицы одного вида по трём осям и их картинки.
#[derive(Default)]
struct MapSet {
    grids: Vec<Option<dsp::Grid>>,
    /// Верх шкалы цвета, дБ — общий для трёх осей, чтобы они сравнивались по цвету.
    top_db: f32,
    /// Правая граница оси X в её единицах (%, с или Гц).
    x_max: f64,
    textures: Vec<Option<egui::TextureHandle>>,
}

struct Maps {
    signal: &'static str,
    /// Диапазон, под который раскрашены картинки.
    range_db: f32,
    /// По [`MapKind`].
    sets: [MapSet; 3],
}

/// Пробный фильтр нижних частот: порядок (PT1..PT3) и частота среза, Гц.
type Trial = (u32, u32);
/// Спектры и задержки `gyroUnfilt` после пробного фильтра, по осям.
type TrialResult = ([Vec<[f64; 2]>; 3], [Option<f64>; 3]);

/// Результат анализа вместе с участком лога, для которого он посчитан.
type ForRange<T> = Option<((usize, usize), T)>;

struct Loaded {
    log: bbl::Log,
    sample_rate: f64,
    axis_fields: Vec<AxisField>,
    /// Поля без разбивки по осям: `(столбец, включено)`.
    other_fields: Vec<(usize, bool)>,
    /// Участок лога, видимый на кривых (номера кадров); по нему считается весь анализ, кроме карт.
    range: (usize, usize),
    /// Просьба вернуть на кривых масштаб «весь лог».
    reset_zoom: bool,
    spectra: ForRange<Spectra>,
    /// Задержка `gyroADC` относительно `gyroUnfilt` по осям, мс.
    delays: ForRange<[Option<f64>; 3]>,
    trial: ForRange<(Trial, TrialResult)>,
    maps: Option<Maps>,
    steps: ForRange<tabs::Steps>,
    motors: ForRange<tabs::Motors>,
    errors: ForRange<[Option<tabs::TrackingError>; 3]>,
    overview: ForRange<tabs::Overview>,
}

/// Дописывает в лог поле `target[ось]` копией `source[ось]`, если его ещё нет.
fn alias(log: &mut bbl::Log, target: &str, source: &str) {
    for a in 0..3 {
        if log.col(&format!("{target}[{a}]")).is_none()
            && let Some(col) = log.col(&format!("{source}[{a}]")).map(<[f32]>::to_vec)
        {
            log.names.push(format!("{target}[{a}]"));
            log.cols.push(col);
        }
    }
}

/// Приводит поля разных прошивок к именам, с которыми работает анализ:
/// дописывает в лог `setpoint[…]` и `gyroUnfilt[…]`, если они записаны под
/// другим именем или их можно восстановить.
fn unify_fields(log: &mut bbl::Log) {
    alias(log, "setpoint", "axisRate"); // INAV
    alias(log, "gyroUnfilt", "gyroRAW"); // Rotorflight
    // Betaflight 4.x и EmuFlight: сырой гироскоп пишется в отладочные поля в режиме GYRO_SCALED.
    let firmware = log.header("Firmware revision").unwrap_or("").to_string();
    if log.header("debug_mode").map(str::trim) == Some("6") && (firmware.starts_with("Betaflight 4.") || firmware.starts_with("EmuFlight")) {
        alias(log, "gyroUnfilt", "debug");
    }

    // Betaflight 3.x задание не пишет — оно восстанавливается из положения стиков по формуле рейтов.
    if log.col("setpoint[0]").is_none() && firmware.starts_with("Betaflight 3.") {
        restore_setpoint(log);
    }

    // Ошибка слежения — отдельным полем: на её спектрограмме и карте шума бывает видно
    // то, что в самом гироскопе тонет в манёврах.
    for a in 0..3 {
        if let (Some(setpoint), Some(gyro)) = (log.col(&format!("setpoint[{a}]")), log.col(&format!("gyroADC[{a}]"))) {
            let error = setpoint.iter().zip(gyro).map(|(s, g)| s - g).collect();
            log.names.push(format!("{TRACKING_ERROR}[{a}]"));
            log.cols.push(error);
        }
    }
}

/// Задание по осям из положения стиков по старой формуле рейтов Betaflight.
fn restore_setpoint(log: &mut bbl::Log) {
    for a in 0..3 {
        let number = |name: &str, index: usize| log.header(name).and_then(|v| v.split(',').nth(index)?.trim().parse::<f32>().ok());
        let (rate_name, expo_name) = if a == 2 { ("rcYawRate", "rcYawExpo") } else { ("rcRate", "rcExpo") };
        let (Some(rc_rate), Some(expo), Some(super_rate), Some(stick)) = (number(rate_name, 0), number(expo_name, 0), number("rates", a), log.col(&format!("rcCommand[{a}]"))) else {
            return;
        };
        let (mut rc_rate, expo, super_rate) = (rc_rate / 100.0, expo / 100.0, super_rate / 100.0);
        if rc_rate > 2.0 {
            rc_rate += 14.54 * (rc_rate - 2.0);
        }
        let setpoint = stick
            .iter()
            .map(|&rc| {
                let stick = (rc / 500.0).clamp(-1.0, 1.0);
                let curved = stick * stick.abs().powi(3) * expo + stick * (1.0 - expo);
                200.0 * rc_rate * curved / (1.0 - stick.abs() * super_rate).clamp(0.01, 1.0)
            })
            .collect();
        log.names.push(format!("setpoint[{a}]"));
        log.cols.push(setpoint);
    }
}

impl Loaded {
    fn new(mut log: bbl::Log) -> Self {
        unify_fields(&mut log);
        let col = |name: String| log.names.iter().position(|n| *n == name);
        let axis_fields: Vec<AxisField> = AXIS_BASES
            .iter()
            .map(|&base| AxisField { base, cols: [0, 1, 2].map(|a| col(format!("{base}[{a}]"))), on: DEFAULT_CURVES.contains(&base) })
            .filter(|f| f.cols[0].is_some())
            .collect();
        let other_fields = (0..log.names.len())
            .filter(|i| !axis_fields.iter().any(|f| f.cols.contains(&Some(*i))))
            .filter(|&i| !["loopIteration", "time"].contains(&log.names[i].as_str()))
            .map(|i| (i, false))
            .collect();
        Self {
            sample_rate: log.sample_rate(),
            range: (0, log.time.len()),
            reset_zoom: false,
            axis_fields,
            other_fields,
            spectra: None,
            delays: None,
            trial: None,
            maps: None,
            steps: None,
            motors: None,
            errors: None,
            overview: None,
            log,
        }
    }

    fn axis_cols(&self, base: &str) -> [Option<&[f32]>; 3] {
        let field = self.axis_fields.iter().find(|f| f.base == base);
        [0, 1, 2].map(|a| Some(self.log.cols[field?.cols[a]?].as_slice()))
    }

    /// Газ в процентах.
    fn throttle_pct(&self) -> Option<Vec<f32>> {
        if let Some(sp) = self.log.col("setpoint[3]") {
            return Some(sp.iter().map(|v| v / 10.0).collect());
        }
        Some(self.log.col("rcCommand[3]")?.iter().map(|v| (v - 1000.0) / 10.0).collect())
    }

    /// Средняя по моторам частота вращения, Гц (об/с), из телеметрии eRPM.
    fn motor_hz(&self) -> Option<Vec<f32>> {
        let cols: Vec<&[f32]> = (0..8).filter_map(|i| self.log.col(&format!("eRPM[{i}]"))).collect();
        let poles: f32 = self.log.header("motor_poles").and_then(|v| v.trim().parse().ok()).unwrap_or(14.0);
        // В логе eRPM записан в сотнях электрических оборотов в минуту.
        let to_hz = 100.0 / (poles / 2.0) / 60.0 / cols.len() as f32;
        (!cols.is_empty()).then(|| (0..self.log.time.len()).map(|i| cols.iter().map(|c| c[i]).sum::<f32>() * to_hz).collect())
    }

    fn spectra(&mut self) -> &Spectra {
        if self.spectra.as_ref().is_none_or(|(range, _)| *range != self.range) {
            let (lo, hi) = self.range;
            let spectra = SPECTRUM_BASES
                .iter()
                .filter(|&&base| self.axis_fields.iter().any(|f| f.base == base))
                .map(|&base| (base, self.axis_cols(base).map(|c| c.map_or(Vec::new(), |c| dsp::welch_db(&c[lo..hi], self.sample_rate)))))
                .collect();
            self.spectra = Some((self.range, spectra));
        }
        &self.spectra.as_ref().unwrap().1
    }

    fn delays(&mut self) -> [Option<f64>; 3] {
        if self.delays.as_ref().is_none_or(|(range, _)| *range != self.range) {
            let (lo, hi) = self.range;
            let (raw, filtered) = (self.axis_cols("gyroUnfilt"), self.axis_cols("gyroADC"));
            let delays = [0, 1, 2].map(|a| dsp::delay_ms(&raw[a]?[lo..hi], &filtered[a]?[lo..hi], self.sample_rate));
            self.delays = Some((self.range, delays));
        }
        self.delays.as_ref().unwrap().1
    }

    fn trial(&mut self, trial: Trial) -> &TrialResult {
        if self.trial.as_ref().is_none_or(|(range, (t, _))| *range != self.range || *t != trial) {
            let ((lo, hi), fs) = (self.range, self.sample_rate);
            let filtered = self.axis_cols("gyroUnfilt").map(|c| c.map(|c| (&c[lo..hi], dsp::lowpass(&c[lo..hi], trial.1 as f64, trial.0, fs))));
            let spectra = [0, 1, 2].map(|a| filtered[a].as_ref().map_or(Vec::new(), |(_, f)| dsp::welch_db(f, fs)));
            let delays = [0, 1, 2].map(|a| filtered[a].as_ref().and_then(|(raw, f)| dsp::delay_ms(raw, f, fs)));
            self.trial = Some((self.range, (trial, (spectra, delays))));
        }
        &self.trial.as_ref().unwrap().1.1
    }

    fn maps(&mut self, ctx: &egui::Context, signal: &'static str, range_db: f32) -> &Maps {
        if self.maps.as_ref().is_none_or(|m| m.signal != signal) {
            let (fs, cols) = (self.sample_rate, self.axis_cols(signal));
            let throttle = self.throttle_pct();
            // Обороты переводятся в проценты от наибольших — так карта общая с картой по газу.
            let motor_hz = self.motor_hz();
            let rpm_max = motor_hz.iter().flatten().copied().fold(1.0, f32::max);
            let rpm_pct: Option<Vec<f32>> = motor_hz.map(|hz| hz.iter().map(|v| v / rpm_max * 100.0).collect());
            let binned = |key: &Option<Vec<f32>>| cols.iter().map(|c| dsp::binned_heatmap((*c)?, key.as_deref()?, fs)).collect();
            let set = |grids: Vec<Option<dsp::Grid>>, x_max: f64| {
                // Верх шкалы — не максимум, а уровень, выше которого лишь 0.5 % ячеек:
                // иначе один удар или краш в логе затемняет всю остальную карту.
                let mut levels: Vec<f32> = grids.iter().flatten().flat_map(|g| &g.db).copied().filter(|v| !v.is_nan()).collect();
                let top_db = if levels.is_empty() {
                    0.0
                } else {
                    let k = levels.len() * 995 / 1000;
                    *levels.select_nth_unstable_by(k, f32::total_cmp).1
                };
                MapSet { grids, top_db, x_max, textures: Vec::new() }
            };
            let sets = [
                set(binned(&throttle), 100.0),
                set(cols.iter().map(|c| dsp::spectrogram((*c)?, &self.log.time, fs)).collect(), *self.log.time.last().unwrap()),
                set(binned(&rpm_pct), rpm_max as f64),
            ];
            self.maps = Some(Maps { signal, range_db: f32::NAN, sets });
        }
        let maps = self.maps.as_mut().unwrap();
        if maps.range_db != range_db {
            maps.range_db = range_db;
            for set in &mut maps.sets {
                let image = |g: &Option<dsp::Grid>| Some(ctx.load_texture("map", grid_image(g.as_ref()?, set.top_db, range_db), egui::TextureOptions::LINEAR));
                set.textures = set.grids.iter().map(image).collect();
            }
        }
        maps
    }
}

/// Картинка матрицы: уровни от `top_db - range_db` до `top_db`, высокие частоты сверху.
fn grid_image(grid: &dsp::Grid, top_db: f32, range_db: f32) -> egui::ColorImage {
    let mut pixels = Vec::with_capacity(grid.w * grid.h);
    for row in 0..grid.h {
        for x in 0..grid.w {
            let db = grid.db[x * grid.h + (grid.h - 1 - row)];
            pixels.push(if db.is_nan() { theme::SURFACE } else { theme::colormap(1.0 - (top_db - db) / range_db) });
        }
    }
    egui::ColorImage::new([grid.w, grid.h], pixels)
}

/// Точки поля в диапазоне кадров `lo..hi`. Если их больше, чем пикселей,
/// каждая колонка пикселей сводится к минимуму и максимуму — форма сигнала
/// сохраняется, а число точек не зависит от длины лога.
fn decimated(time: &[f64], y: &[f32], (lo, hi): (usize, usize), pixels: usize) -> Vec<[f64; 2]> {
    let (time, y) = (&time[lo..hi], &y[lo..hi]);
    let bucket = time.len() / pixels.max(1);
    if bucket < 4 {
        return time.iter().zip(y).map(|(&t, &v)| [t, v as f64]).collect();
    }
    let mut points = Vec::with_capacity(2 * pixels + 2);
    for (ts, ys) in time.chunks(bucket).zip(y.chunks(bucket)) {
        let (mut i_min, mut i_max) = (0, 0);
        for (i, &v) in ys.iter().enumerate() {
            if v < ys[i_min] {
                i_min = i;
            }
            if v > ys[i_max] {
                i_max = i;
            }
        }
        for i in [i_min.min(i_max), i_min.max(i_max)] {
            points.push([ts[i], ys[i] as f64]);
        }
    }
    points
}

/// Высота одного из `count` графиков, делящих оставшееся место по вертикали.
fn row_height(ui: &egui::Ui, count: usize) -> f32 {
    (ui.available_height() - ui.spacing().item_spacing.y * (count - 1) as f32) / count as f32
}

/// Заготовка графика с общими настройками.
fn new_plot<'a>(id: impl egui::AsId) -> Plot<'a> {
    // Подписи делений по X видны уже при шаге от 50 px, а не только у самых редких линий сетки.
    let x_axis = AxisHints::new_x().label_spacing(egui::Rangef::new(30.0, 50.0));
    Plot::new(id).custom_x_axes(vec![x_axis]).y_axis_min_width(Y_AXIS_WIDTH).grid_spacing(egui::Rangef::new(16.0, 60.0)).grid_color(theme::MUTED)
}

/// Частоты настроенных фильтров из заголовка лога: `(параметр, Гц)`.
fn filter_marks(log: &bbl::Log) -> Vec<(String, f64)> {
    const PREFIXES: [&str; 8] = ["gyro_lpf", "dterm_lpf", "dyn_notch_min_hz", "dyn_notch_max_hz", "gyro_notch_hz", "dterm_notch_hz", "yaw_lowpass_hz", "adrc_gyro_lpf_hz"];
    let mut marks = Vec::new();
    for (name, value) in log.headers.iter().filter(|(n, _)| n.ends_with("_hz") && PREFIXES.iter().any(|p| n.starts_with(p))) {
        for hz in value.split(',').filter_map(|v| v.trim().parse::<f64>().ok()).filter(|&hz| hz > 0.0) {
            if !marks.contains(&(name.clone(), hz)) {
                marks.push((name.clone(), hz));
            }
        }
    }
    marks
}

/// Цветная метка серии: кружок цвета линии; подпись рядом пишется обычным цветом текста.
fn swatch(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, theme::series_color(name));
}

fn line<'a>(name: &str, points: Vec<[f64; 2]>) -> Line<'a> {
    Line::new(name, PlotPoints::new(points)).color(theme::series_color(name)).width(1.5)
}

/// Множитель и единица измерения для полей, которые в логе записаны в сотых долях.
fn unit(name: &str) -> (f64, &'static str) {
    match name {
        "vbatLatest" => (0.01, ", В"),
        "amperageLatest" => (0.01, ", А"),
        _ => (1.0, ""),
    }
}

/// «Roll 1.9 мс, Pitch 2.0 мс, Yaw нет данных».
fn per_axis_ms(values: [Option<f64>; 3]) -> String {
    let text = |a: usize| values[a].map_or(format!("{} нет данных", AXES[a]), |ms| format!("{} {ms:.1} мс", AXES[a]));
    [text(0), text(1), text(2)].join(", ")
}

fn show_curves(ui: &mut egui::Ui, loaded: &mut Loaded) {
    let log = &loaded.log;
    let others: Vec<usize> = loaded.other_fields.iter().filter(|(_, on)| *on).map(|(i, _)| *i).collect();
    let rows = 3 + !others.is_empty() as usize;
    let height = row_height(ui, rows);
    let mut range = loaded.range;
    for row in 0..rows {
        let mut plot = new_plot(("curves", row)).height(height).link_axis("curves", [true, false]).link_cursor("curves", [true, false]).y_axis_label(AXES.get(row).copied().unwrap_or("Прочее"));
        if row == rows - 1 {
            plot = plot.x_axis_label("время, с");
        }
        plot.show(ui, |plot| {
            if loaded.reset_zoom {
                plot.set_auto_bounds([true, true]);
            }
            if row == 0 {
                // Пока масштаб автоматический, показываем весь лог: иначе границы
                // подстраивались бы под уже обрезанные по ним же точки.
                range = if plot.auto_bounds().x {
                    (0, log.time.len())
                } else {
                    let (x_min, x_max) = (plot.plot_bounds().min()[0], plot.plot_bounds().max()[0]);
                    let lo = log.time.partition_point(|&t| t < x_min).saturating_sub(1);
                    (lo, (log.time.partition_point(|&t| t <= x_max) + 1).min(log.time.len()).max(lo))
                };
            }
            let pixels = plot.response().rect.width() as usize;
            let cols: Vec<usize> = match row {
                0..3 => loaded.axis_fields.iter().filter(|f| f.on).filter_map(|f| f.cols[row]).collect(),
                _ => others.clone(),
            };
            for i in cols {
                let (scale, suffix) = unit(&log.names[i]);
                let points = decimated(&log.time, &log.cols[i], range, pixels).into_iter().map(|[t, v]| [t, v * scale]).collect();
                plot.line(line(&log.names[i], points).name(format!("{}{suffix}", log.names[i])));
            }
            for (time, event) in &log.events {
                plot.vline(VLine::new(event, *time).color(theme::MUTED).style(LineStyle::dashed_dense()).width(1.0));
            }
        });
    }
    loaded.range = range;
    loaded.reset_zoom = false;
}

fn show_spectrum(ui: &mut egui::Ui, loaded: &mut Loaded, compare: Option<&mut Loaded>, view: &mut View) {
    let (lo, hi) = loaded.range;
    let span = format!("Участок {:.2}–{:.2} с. Чтобы выбрать другой, измените масштаб на вкладке «Кривые».", loaded.log.time[lo], loaded.log.time[hi.max(lo + 1) - 1]);
    let nyquist = loaded.sample_rate / 2.0;
    let marks: Vec<(String, f64)> = filter_marks(&loaded.log).into_iter().filter(|(_, hz)| *hz < nyquist).collect();
    let delays = loaded.delays();
    let has_raw = loaded.axis_fields.iter().any(|f| f.base == "gyroUnfilt");
    let trial = (view.trial_on && has_raw).then(|| loaded.trial((view.trial_order, view.trial_hz)).clone());
    let spectra = loaded.spectra();
    let compared = compare.map(|c| c.spectra());

    ui.horizontal_wrapped(|ui| {
        for (base, _) in spectra {
            swatch(ui, base);
            ui.label(*base);
        }
        if trial.is_some() {
            swatch(ui, TRIAL_FILTER);
            ui.label(TRIAL_FILTER);
        }
        ui.separator();
        ui.colored_label(theme::MUTED, span);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Задержка фильтров гироскопа");
        ui.colored_label(theme::MUTED, if has_raw { per_axis_ms(delays) } else { "нет поля gyroUnfilt в логе".into() });
        if has_raw {
            ui.separator();
            ui.checkbox(&mut view.trial_on, "Пробный фильтр на gyroUnfilt");
            if view.trial_on {
                egui::ComboBox::from_id_salt("trial_order").selected_text(format!("PT{}", view.trial_order)).show_ui(ui, |ui| {
                    for order in 1..=3 {
                        ui.selectable_value(&mut view.trial_order, order, format!("PT{order}"));
                    }
                });
                ui.add(egui::Slider::new(&mut view.trial_hz, 20..=(nyquist as u32).max(21)).suffix(" Гц"));
                if let Some((_, trial_delays)) = &trial {
                    ui.colored_label(theme::MUTED, format!("задержка с ним: {}", per_axis_ms(*trial_delays)));
                }
            }
        }
    });

    let height = row_height(ui, 3);
    for (a, axis) in AXES.iter().enumerate() {
        let mut plot = new_plot(("spectrum", a)).height(height).link_axis("spectrum", [true, true]).link_cursor("spectrum", [true, false]).y_axis_label(format!("{axis}, дБ"));
        if a == 2 {
            plot = plot.x_axis_label("частота, Гц");
        }
        plot.show(ui, |plot| {
            for (base, by_axis) in spectra {
                plot.line(line(base, by_axis[a].clone()));
            }
            if let Some((trial_spectra, _)) = &trial {
                plot.line(line(TRIAL_FILTER, trial_spectra[a].clone()));
            }
            for (base, by_axis) in compared.into_iter().flatten() {
                plot.line(line(base, by_axis[a].clone()).name(format!("{base}, сравнение")).style(LineStyle::dashed_loose()));
            }
            // Подписи идут лесенкой, чтобы соседние частоты не накладывались. Высота
            // берётся от самих спектров, а не от границ графика: иначе подписи раздвигали
            // бы границы, а те в ответ поднимали бы подписи.
            let levels = || spectra.iter().flat_map(|(_, by_axis)| &by_axis[a]).map(|p| p[1]);
            let (bottom, top) = (levels().fold(f64::INFINITY, f64::min), levels().fold(f64::NEG_INFINITY, f64::max));
            let step = (top - bottom) * 0.07;
            for (k, (name, hz)) in marks.iter().enumerate() {
                plot.vline(VLine::new(name, *hz).color(theme::MUTED).style(LineStyle::dashed_dense()).width(1.0));
                if a == 0 && top.is_finite() {
                    let label = Text::new(name, PlotPoint::new(*hz, top - step * (k % 5) as f64), format!(" {name} {hz:.0}"));
                    plot.text(label.color(theme::MUTED).anchor(egui::Align2::LEFT_TOP));
                }
            }
        });
    }
}

/// Шкала цвета карт: полоска палитры с подписями крайних уровней.
fn colorbar(ui: &mut egui::Ui, top_db: f32, range_db: f32) {
    ui.colored_label(theme::MUTED, format!("{:.0} дБ", top_db - range_db));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 10.0), egui::Sense::hover());
    let steps = 64;
    for i in 0..steps {
        let x = |k: usize| rect.left() + rect.width() * k as f32 / steps as f32;
        let cell = egui::Rect::from_min_max(egui::pos2(x(i), rect.top()), egui::pos2(x(i + 1), rect.bottom()));
        ui.painter().rect_filled(cell, 0.0, theme::colormap(i as f32 / (steps - 1) as f32));
    }
    ui.colored_label(theme::MUTED, format!("{top_db:.0} дБ"));
}

/// Подпись под курсором на карте: положение по осям и уровень в ячейке матрицы.
fn map_readout(grid: Option<&dsp::Grid>, unit: &str, x_max: f64, f_max: f64, x: f64, hz: f64) -> String {
    let level = grid.and_then(|g| {
        let (col, row) = (x / x_max * g.w as f64, hz / f_max * (g.h - 1) as f64);
        ((0.0..g.w as f64).contains(&col) && (0.0..g.h as f64).contains(&row)).then(|| g.db[col as usize * g.h + row as usize])
    });
    match level.filter(|db| !db.is_nan()) {
        Some(db) => format!("{x:.1} {unit}, {hz:.0} Гц: {db:.0} дБ"),
        None => format!("{x:.1} {unit}, {hz:.0} Гц: нет данных"),
    }
}

/// Карты шума по трём осям: спектрограмма (оси друг под другом) или шум
/// от режима — газа либо оборотов (оси рядом).
fn show_maps(ui: &mut egui::Ui, loaded: &mut Loaded, spectrogram: bool, view: &mut View) {
    let f_max = loaded.sample_rate / 2.0;
    if !loaded.axis_fields.iter().any(|f| f.base == view.map_signal) {
        view.map_signal = loaded.axis_fields.first().map_or("", |f| f.base);
    }
    let bases: Vec<&'static str> = loaded.axis_fields.iter().map(|f| f.base).collect();
    let has_rpm = loaded.log.col("eRPM[0]").is_some();
    let kind = match (spectrogram, view.by_rpm && has_rpm) {
        (true, _) => MapKind::Time,
        (false, true) => MapKind::Rpm,
        (false, false) => MapKind::Throttle,
    };
    let set = &loaded.maps(ui.ctx(), view.map_signal, view.map_range_db).sets[kind as usize];
    let x_label = ["газ, %", "время, с", "обороты моторов, Гц"][kind as usize];
    let harmonics = matches!(kind, MapKind::Rpm) && view.harmonics;

    ui.horizontal(|ui| {
        if !spectrogram && has_rpm {
            ui.selectable_value(&mut view.by_rpm, false, "По газу");
            ui.selectable_value(&mut view.by_rpm, true, "По оборотам");
            ui.separator();
        }
        ui.label("Сигнал");
        egui::ComboBox::from_id_salt("map_signal").selected_text(view.map_signal).show_ui(ui, |ui| {
            for base in bases {
                ui.selectable_value(&mut view.map_signal, base, base);
            }
        });
        ui.separator();
        ui.label("Диапазон");
        ui.add(egui::Slider::new(&mut view.map_range_db, 20.0..=100.0).suffix(" дБ").fixed_decimals(0));
        ui.separator();
        colorbar(ui, set.top_db, view.map_range_db);
        if matches!(kind, MapKind::Rpm) {
            ui.separator();
            ui.checkbox(&mut view.harmonics, "Гармоники оборотов").on_hover_text(
                "Штриховые линии 1×, 2×, 3× — где должен лежать шум, кратный оборотам моторов. Яркая полоса вдоль такой линии — шум от моторов и пропеллеров; полоса на постоянной частоте — резонанс рамы.",
            );
        }
    });

    let plot_axis = |ui: &mut egui::Ui, a: usize, height: f32, with_x_label: bool| {
        let Some(texture) = set.textures.get(a).and_then(Option::as_ref) else {
            ui.colored_label(theme::MUTED, format!("{}: в логе нет этого сигнала, газа или оборотов", AXES[a]));
            return;
        };
        let mut plot = new_plot(("map", kind as usize, a))
            .height(height)
            .link_axis(egui::Id::new(("map", kind as usize)), [true, true])
            .link_cursor(egui::Id::new(("map", kind as usize)), [true, true])
            .set_margin_fraction(egui::Vec2::ZERO)
            .show_grid(false)
            .y_axis_label(format!("{}, Гц", AXES[a]));
        if with_x_label {
            plot = plot.x_axis_label(x_label);
        }
        // В углу — что под курсором: положение по осям и уровень в этой ячейке.
        let grid = set.grids[a].as_ref();
        let unit = ["%", "с", "Гц"][kind as usize];
        let readout = move |point: &PlotPoint, _: &egui_plot::PlotBounds| map_readout(grid, unit, set.x_max, f_max, point.x, point.y);
        plot = plot.coordinates_formatter(egui_plot::Corner::RightTop, egui_plot::CoordinatesFormatter::new(readout));
        plot.show(ui, |plot| {
            let center = PlotPoint::new(set.x_max / 2.0, f_max / 2.0);
            plot.image(PlotImage::new(AXES[a], texture.id(), center, [set.x_max as f32, f_max as f32]));
            if harmonics {
                for harmonic in 1..=3 {
                    let name = format!("{harmonic}×");
                    let end = set.x_max.min(f_max / harmonic as f64);
                    let points = vec![[0.0, 0.0], [end, end * harmonic as f64]];
                    plot.line(Line::new(format!("{name} оборотов"), PlotPoints::new(points)).color(theme::INK).style(LineStyle::dashed_dense()).width(1.0));
                    // Подпись — у верхнего конца линии, внутри поля графика.
                    let at = PlotPoint::new(end * 0.9, end * 0.9 * harmonic as f64);
                    plot.text(Text::new(&name, at, format!(" {name}")).color(theme::INK).anchor(egui::Align2::LEFT_CENTER));
                }
            }
        });
    };
    if spectrogram {
        let height = row_height(ui, 3);
        for a in 0..3 {
            plot_axis(ui, a, height, a == 2);
        }
    } else {
        let height = ui.available_height();
        ui.columns(3, |columns| {
            for (a, ui) in columns.iter_mut().enumerate() {
                plot_axis(ui, a, height, true);
            }
        });
    }
}

/// Пара «подпись — значение» в строке состояния.
fn fact(ui: &mut egui::Ui, label: &str, value: String) {
    ui.colored_label(theme::MUTED, label);
    ui.label(value);
    ui.add_space(8.0);
}

/// Настройки вкладок, общие для всех логов.
struct View {
    /// Сигнал для карты шума и спектрограммы.
    map_signal: &'static str,
    /// Динамический диапазон палитры карт, дБ.
    map_range_db: f32,
    /// Карта шума по оборотам, а не по газу.
    by_rpm: bool,
    /// Линии гармоник оборотов на карте по оборотам.
    harmonics: bool,
    trial_on: bool,
    trial_order: u32,
    trial_hz: u32,
    /// Поиск по названию параметра на вкладке «Настройки».
    settings_filter: String,
    /// Показывать только параметры, отличающиеся у лога сравнения.
    only_different: bool,
}

struct App {
    path: PathBuf,
    logs: Vec<Loaded>,
    /// Второй лог для сравнения: `(имя файла, лог)`.
    compare: Option<(String, Loaded)>,
    current: usize,
    error: Option<String>,
    /// Сообщение о последнем действии в строке состояния.
    notice: Option<String>,
    tab: Tab,
    view: View,
    /// Режим «снять и выйти»: через сколько кадров сохранить снимок вкладки и закрыть окно.
    snapshot_in: Option<u32>,
}

/// Все логи с данными из файла.
fn load(path: &Path) -> Result<Vec<Loaded>, String> {
    let data = std::fs::read(path).map_err(|e| format!("Не удалось прочитать {}: {e}", path.display()))?;
    let mut errors = vec![format!("В файле {} нет логов с данными", path.display())];
    let logs: Vec<Loaded> = bbl::parse_file(&data).into_iter().filter_map(|l| l.map_err(|e| errors.push(e)).ok()).filter(|l| l.time.len() > 1).map(Loaded::new).collect();
    if logs.is_empty() { Err(errors.join("; ")) } else { Ok(logs) }
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

fn pick_log() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("Blackbox", &["bbl", "bfl", "txt"]).pick_file()
}

impl App {
    fn open(&mut self, path: &Path) {
        (self.error, self.notice) = (None, None);
        match load(path) {
            Ok(logs) => {
                self.path = path.to_path_buf();
                self.logs = logs;
                self.current = 0;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Берёт для сравнения первую запись из файла.
    fn open_compare(&mut self, path: &Path) {
        (self.error, self.notice) = (None, None);
        match load(path) {
            Ok(mut logs) => self.compare = Some((file_name(path), logs.swap_remove(0))),
            Err(e) => self.error = Some(e),
        }
    }

    /// Сохраняет снимок окна в PNG рядом с логом: `<лог>_<вкладка>.png`.
    fn save_screenshot(&mut self, image: &egui::ColorImage) {
        let slug = TABS.iter().find(|t| t.0 == self.tab).map_or("", |t| t.2);
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let target = self.path.with_file_name(format!("{stem}_{slug}.png"));
        let saved = image::save_buffer(&target, image.as_raw(), image.width() as u32, image.height() as u32, image::ColorType::Rgba8);
        match saved {
            Ok(()) => self.notice = Some(format!("Снимок сохранён: {}", target.display())),
            Err(e) => self.error = Some(format!("Не удалось сохранить снимок в {}: {e}", target.display())),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    fn new() -> Self {
        let view = View { map_signal: "gyroADC", map_range_db: 50.0, by_rpm: false, harmonics: true, trial_on: false, trial_order: 1, trial_hz: 100, settings_filter: String::new(), only_different: false };
        Self { path: PathBuf::new(), logs: Vec::new(), compare: None, current: 0, error: None, notice: None, tab: Tab::Overview, view, snapshot_in: None }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        if let Some(path) = ui.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
            self.open(&path);
        }
        let screenshot = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = screenshot {
            self.save_screenshot(&image);
            if self.snapshot_in.is_some() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(frames) = &mut self.snapshot_in {
            // Несколько кадров уходит на расчёт и установку масштаба графиков.
            if *frames == 1 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            *frames = frames.saturating_sub(1);
            ui.ctx().request_repaint();
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            // С переносом: при выборе записи и в узком окне вкладки уходят на вторую строку, а не за край.
            ui.horizontal_wrapped(|ui| {
                if ui.button("Открыть лог…").clicked()
                    && let Some(path) = pick_log()
                {
                    self.open(&path);
                }
                if !self.logs.is_empty() {
                    if self.compare.is_some() {
                        if ui.button("Убрать сравнение").clicked() {
                            self.compare = None;
                        }
                    } else if ui.button("Сравнить с…").clicked()
                        && let Some(path) = pick_log()
                    {
                        self.open_compare(&path);
                    }
                    if ui.button("Сохранить снимок").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    }
                }
                if self.logs.len() > 1 {
                    egui::ComboBox::from_id_salt("log").selected_text(format!("Запись {} из {}", self.current + 1, self.logs.len())).show_ui(ui, |ui| {
                        for (i, l) in self.logs.iter().enumerate() {
                            ui.selectable_value(&mut self.current, i, format!("Запись {}, {:.0} с", i + 1, l.log.time.last().unwrap_or(&0.0)));
                        }
                    });
                }
                ui.separator();
                for (tab, name, _) in TABS {
                    ui.selectable_value(&mut self.tab, tab, name);
                }
            });
            ui.add_space(4.0);
        });

        let mut whole_log = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| match (&self.error, self.logs.get(self.current)) {
                (Some(e), _) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                (None, Some(l)) => {
                    fact(ui, "Лог", file_name(&self.path));
                    fact(ui, "Прошивка", l.log.header("Firmware revision").unwrap_or("неизвестна").to_string());
                    fact(ui, "Запись", format!("{:.0} Гц", l.sample_rate));
                    fact(ui, "Длительность", format!("{:.1} с", l.log.time.last().unwrap_or(&0.0)));
                    fact(ui, "Разрывов записи", l.log.corrupt.to_string());
                    if l.range != (0, l.log.time.len()) {
                        let (lo, hi) = l.range;
                        fact(ui, "Анализируется участок", format!("{:.2}–{:.2} с", l.log.time[lo], l.log.time[hi.max(lo + 1) - 1]));
                        whole_log = ui.button("Весь лог").clicked();
                    }
                    if let Some((name, _)) = &self.compare {
                        fact(ui, "Сравнение, штриховые линии", name.clone());
                    }
                    if let Some(notice) = &self.notice {
                        ui.label(notice);
                    }
                }
                (None, None) => {}
            });
        });

        if let (true, Some(l)) = (whole_log, self.logs.get_mut(self.current)) {
            (l.range, l.reset_zoom) = ((0, l.log.time.len()), true);
        }

        let Some(loaded) = self.logs.get_mut(self.current) else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.colored_label(theme::MUTED, "Перетащите сюда файл лога (.bbl) или нажмите «Открыть лог…»"));
            });
            return;
        };

        if self.tab == Tab::Curves {
            egui::Panel::left("fields").show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.colored_label(theme::MUTED, "По осям");
                    for f in &mut loaded.axis_fields {
                        ui.horizontal(|ui| {
                            swatch(ui, f.base);
                            ui.checkbox(&mut f.on, f.base);
                        });
                    }
                    ui.add_space(8.0);
                    ui.colored_label(theme::MUTED, "Прочее");
                    for (i, on) in &mut loaded.other_fields {
                        ui.horizontal(|ui| {
                            swatch(ui, &loaded.log.names[*i]);
                            ui.checkbox(on, &loaded.log.names[*i]);
                        });
                    }
                });
            });
        }

        let compare = self.compare.as_mut().map(|(_, c)| c);
        let view = &mut self.view;
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Overview => tabs::show_overview(ui, loaded),
            Tab::Curves => show_curves(ui, loaded),
            Tab::Spectrum => show_spectrum(ui, loaded, compare, view),
            Tab::Heatmap => show_maps(ui, loaded, false, view),
            Tab::Spectrogram => show_maps(ui, loaded, true, view),
            Tab::Step => tabs::show_step(ui, loaded, compare),
            Tab::Error => tabs::show_error(ui, loaded),
            Tab::Motors => tabs::show_motors(ui, loaded),
            Tab::Settings => tabs::show_settings(ui, loaded, compare.map(|c| &*c), view),
        });
    }
}

/// Печатает строку на стандартный вывод. Если читатель закрыл поток (`| head`),
/// программа спокойно завершается, а не падает с ошибкой записи.
fn say(text: std::fmt::Arguments) {
    use std::io::Write;
    if writeln!(std::io::stdout(), "{text}").is_err() {
        std::process::exit(0);
    }
}

/// Печатает запись в CSV: время от начала в микросекундах и все поля как в логе.
fn write_csv(log: &bbl::Log) {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    // Ошибка записи значит, что читатель закрыл поток (`| head`) — молча прекращаем.
    let mut line = |text: String| writeln!(out, "{text}").is_ok();
    if !line(format!("time_us,{}", log.names.join(","))) {
        return;
    }
    for (row, time) in log.time.iter().enumerate() {
        let values: Vec<String> = log.cols.iter().map(|c| c[row].to_string()).collect();
        if !line(format!("{:.0},{}", time * 1e6, values.join(","))) {
            return;
        }
    }
}

fn main() -> eframe::Result {
    let mut app = App::new();
    // Аргументы: лог; вкладка при запуске либо `report` (напечатать находки
    // по каждой записи и выйти, без окна) либо `csv` (выгрузить запись
    // в CSV на стандартный вывод) либо `response` (отклик на ступеньку и частотная
    // характеристика первой записи таблицей); лог для сравнения.
    // Вкладка с окончанием `+save` — сохранить её снимок рядом с логом и выйти.
    let mut args = std::env::args().skip(1);
    if let Some(path) = args.next() {
        app.open(Path::new(&path));
    }
    let mut mode = args.next();
    if let Some(tab) = mode.as_deref().and_then(|m| m.strip_suffix("+save")) {
        (mode, app.snapshot_in) = (Some(tab.to_string()), Some(20));
    }
    // `вкладка@сигнал` — сигнал для карты шума и спектрограммы, например `spectrogram@ошибка`.
    if let Some((tab, signal)) = mode.clone().as_deref().and_then(|m| m.split_once('@')) {
        mode = Some(tab.to_string());
        app.view.map_signal = AXIS_BASES.iter().find(|b| **b == signal).copied().unwrap_or(app.view.map_signal);
    }
    if mode.as_deref() == Some("csv") {
        // Третий аргумент — номер записи в файле, по умолчанию первая.
        let index = args.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1);
        match app.logs.get(index.wrapping_sub(1)) {
            Some(loaded) => write_csv(&loaded.log),
            None => eprintln!("{}", app.error.unwrap_or(format!("В файле нет записи {index}"))),
        }
        return Ok(());
    }
    if mode.as_deref() == Some("response") {
        if let Some(loaded) = app.logs.first_mut() {
            tabs::print_response(loaded);
        }
        return Ok(());
    }
    if mode.as_deref() == Some("report") {
        for (i, loaded) in app.logs.iter_mut().enumerate() {
            say(format_args!("Запись {}: {:.0} с, {:.0} Гц, кадров {}, разрывов записи {}", i + 1, loaded.log.time.last().unwrap_or(&0.0), loaded.sample_rate, loaded.log.time.len(), loaded.log.corrupt));
            for (important, text) in &tabs::overview(loaded).findings {
                say(format_args!("  [{}] {text}", if *important { "важно" } else { "заметка" }));
            }
            for (time, event) in &loaded.log.events {
                say(format_args!("  [событие] {time:.2} с: {event}"));
            }
        }
        if let Some(e) = &app.error {
            eprintln!("{e}");
        }
        return Ok(());
    }
    match mode.as_deref() {
        // Варианты вкладок с включёнными переключателями — для снимков окна при проверке.
        Some("rpm") => (app.tab, app.view.by_rpm) = (Tab::Heatmap, true),
        Some("trial") => (app.tab, app.view.trial_on) = (Tab::Spectrum, true),
        mode => app.tab = TABS.iter().find(|t| Some(t.2) == mode).map_or(app.tab, |t| t.0),
    }
    if let Some(path) = args.next() {
        app.open_compare(Path::new(&path));
    }
    // Приложение лежит в общей ячейке: если первая попытка открыть окно не удалась
    // до его создания, вторая заберёт его оттуда же.
    let slot = std::rc::Rc::new(std::cell::RefCell::new(Some(app)));
    let run = |renderer| {
        let slot = slot.clone();
        let options = eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]), renderer, ..Default::default() };
        eframe::run_native(
            "FPV Log Analyzer",
            options,
            Box::new(move |cc| {
                theme::apply(&cc.egui_ctx);
                Ok(Box::new(slot.borrow_mut().take().expect("приложение создаётся один раз")))
            }),
        )
    };
    match run(eframe::Renderer::Wgpu) {
        // Видеокарта или драйвер не дали Vulkan / DirectX 12 / Metal — рисуем через OpenGL.
        Err(_) if slot.borrow().is_some() => run(eframe::Renderer::Glow),
        result => result,
    }
}

/// Проверки интерфейса без окна: вкладки, зум, строка состояния.
/// Пропускаются, если образца лога нет на диске.
#[cfg(test)]
mod ui_tests {
    use egui_kittest::{Harness, kittest::Queryable};

    use super::*;

    fn harness() -> Option<Harness<'static, App>> {
        let mut app = App::new();
        app.open(Path::new(&(std::env::var("HOME").unwrap() + "/storage/adrc-8ksal8-goodtune/ADRC_Good_Tune.bbl")));
        if app.logs.is_empty() {
            return None;
        }
        Some(Harness::builder().with_size([1400.0, 900.0]).build_ui_state(|ui, app: &mut App| app.draw(ui), app))
    }

    fn whole_log(app: &App) -> bool {
        app.logs[0].range == (0, app.logs[0].log.time.len())
    }

    #[test]
    fn every_tab_opens() {
        let Some(mut harness) = harness() else { return };
        for (tab, name, _) in TABS {
            harness.get_by_label(name).click();
            harness.run_steps(4);
            assert!(harness.state().tab == tab, "вкладка «{name}» не открылась");
        }
        // Переключатели внутри вкладок тоже должны отрисовываться без падений.
        harness.state_mut().tab = Tab::Heatmap;
        harness.state_mut().view.by_rpm = true;
        harness.run_steps(4);
        harness.state_mut().tab = Tab::Spectrum;
        harness.state_mut().view.trial_on = true;
        harness.run_steps(4);
        assert!(harness.query_by_label_contains("задержка с ним").is_some());
    }

    #[test]
    fn zoom_selects_range_and_button_resets_it() {
        let Some(mut harness) = harness() else { return };
        harness.get_by_label("Кривые").click();
        harness.run_steps(4);
        assert!(whole_log(harness.state()));
        assert!(harness.query_by_label("Весь лог").is_none());

        // Зум над графиком Roll сужает участок анализа.
        harness.hover_at(egui::pos2(800.0, 200.0));
        harness.run_steps(2);
        harness.event(egui::Event::Zoom(4.0));
        harness.run_steps(4);
        let (lo, hi) = harness.state().logs[0].range;
        let frames = harness.state().logs[0].log.time.len();
        assert!(hi - lo < frames / 2, "после зума участок {lo}..{hi} из {frames}");
        assert!(harness.query_by_label("Анализируется участок").is_some());

        // Анализ на других вкладках идёт по этому участку.
        harness.get_by_label("Спектр").click();
        harness.run_steps(4);
        assert_eq!(harness.state().logs[0].spectra.as_ref().unwrap().0, (lo, hi));

        harness.get_by_label("Весь лог").click();
        harness.run_steps(4);
        assert!(whole_log(harness.state()));
        harness.get_by_label("Кривые").click();
        harness.run_steps(6);
        assert!(whole_log(harness.state()), "масштаб кривых не вернулся ко всему логу");
        assert!(harness.query_by_label("Весь лог").is_none());
    }

    /// Каждая вкладка на каждом образце лога из `~/storage/fpv-log-samples`:
    /// разные прошивки и наборы полей не должны ронять интерфейс.
    /// Долгий, запускается отдельно: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn every_tab_on_every_sample_log() {
        fn logs_in(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                let ext = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();
                if path.is_dir() {
                    logs_in(&path, out);
                } else if ["bbl", "bfl", "txt"].contains(&ext.as_str()) {
                    out.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        logs_in(Path::new(&(std::env::var("HOME").unwrap() + "/storage/fpv-log-samples")), &mut paths);
        paths.sort();
        let mut opened = 0;
        for path in &paths {
            let mut app = App::new();
            app.open(path);
            if app.logs.is_empty() {
                continue;
            }
            opened += 1;
            let records = app.logs.len();
            let mut harness = Harness::builder().with_size([1400.0, 900.0]).build_ui_state(|ui, app: &mut App| app.draw(ui), app);
            for record in 0..records.min(3) {
                harness.state_mut().current = record;
                for (tab, ..) in TABS {
                    harness.state_mut().tab = tab;
                    harness.state_mut().view.by_rpm = record % 2 == 0;
                    harness.state_mut().view.trial_on = true;
                    harness.run_steps(3);
                }
            }
        }
        eprintln!("открыто логов: {opened} из {}", paths.len());
    }

    #[test]
    fn map_readout_reports_cell_level() {
        let grid = dsp::Grid { w: 2, h: 3, db: vec![1.0, 2.0, 3.0, f32::NAN, 5.0, 6.0] };
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 25.0, 250.0), "25.0 %, 250 Гц: 2 дБ");
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 75.0, 10.0), "75.0 %, 10 Гц: нет данных");
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 150.0, 10.0), "150.0 %, 10 Гц: нет данных");
    }
}

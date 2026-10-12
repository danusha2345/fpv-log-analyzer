// В готовой сборке под Windows не открывать окно консоли рядом с программой.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
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
/// Расчётное поле: задание минус гироскоп. Называется по-английски, как и поля лога.
const TRACKING_ERROR: &str = "error";
/// Что включено на кривых сразу после загрузки.
const DEFAULT_CURVES: [&str; 2] = ["gyroADC", "setpoint"];
/// Сигналы, для которых строится спектр.
const SPECTRUM_BASES: [&str; 3] = ["gyroUnfilt", "gyroADC", "axisD"];
/// Ширина поля под подписи оси Y — одинаковая, чтобы графики друг под другом совпадали по X.
const Y_AXIS_WIDTH: f32 = 56.0;
/// Ключ серии пробного фильтра (по нему выбирается цвет); подпись — [`trial_filter_label`].
const TRIAL_FILTER: &str = "trial filter";

fn trial_filter_label() -> &'static str {
    t!("пробный фильтр", "trial filter")
}

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

/// Вкладки по порядку: `(вкладка, название по-русски, по-английски, имя для командной строки и файлов снимков)`.
const TABS: [(Tab, &str, &str, &str); 9] = [
    (Tab::Overview, "Обзор", "Overview", "overview"),
    (Tab::Curves, "Кривые", "Curves", "curves"),
    (Tab::Spectrum, "Спектр", "Spectrum", "spectrum"),
    (Tab::Heatmap, "Карта шума", "Noise map", "heatmap"),
    (Tab::Spectrogram, "Спектрограмма", "Spectrogram", "spectrogram"),
    (Tab::Step, "Отклик", "Response", "step"),
    (Tab::Error, "Ошибка", "Error", "error"),
    (Tab::Motors, "Моторы", "Motors", "motors"),
    (Tab::Settings, "Настройки", "Settings", "settings"),
];

/// Название вкладки на текущем языке.
fn tab_name(tab: &(Tab, &'static str, &'static str, &'static str)) -> &'static str {
    if i18n::english() { tab.2 } else { tab.1 }
}

/// Текст события лога на текущем языке.
fn event_text(event: &bbl::Event) -> String {
    match event {
        bbl::Event::SyncBeep => t!("Сигнал синхронизации", "Sync beep").into(),
        bbl::Event::InflightAdjustment => t!("Подстройка в полёте", "In-flight adjustment").into(),
        bbl::Event::LoggingResumed => t!("Запись возобновлена", "Logging resumed").into(),
        bbl::Event::Disarm(reason) => tf!("Дизарм, причина {}", "Disarm, reason {}", reason),
        bbl::Event::FlightModes { before, now } => tf!("Режимы полёта: {} → {}", "Flight modes: {} → {}", before, now),
        bbl::Event::EndOfLog => t!("Конец записи", "End of log").into(),
    }
}

/// Как делить отклик на ступеньку на вкладке «Отклик».
#[derive(Clone, Copy, PartialEq)]
enum StepSplit {
    None,
    /// Плавные и резкие движения стиков.
    Rate,
    /// Малый, средний и большой газ.
    Throttle,
}

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
    /// Линии поверх карты: срезы фильтров и гармоники оборотов.
    overlays: Vec<Overlay>,
    textures: Vec<Option<egui::TextureHandle>>,
}

/// Линия поверх карты шума.
struct Overlay {
    label: String,
    /// Точки `[x карты; частота, Гц]`. Там, где линия уходит выше верхней частоты
    /// карты, стоит `NaN` — в этих местах она прерывается.
    points: Vec<[f64; 2]>,
    /// Гармоника оборотов (рисуется пунктиром), а не настройка фильтра (штрих).
    harmonic: bool,
}

struct Maps {
    signal: &'static str,
    /// Диапазон, под который раскрашены картинки.
    range_db: f32,
    /// По [`MapKind`].
    sets: [MapSet; 3],
}

/// Пробный фильтр: две ступени подряд, у каждой вид (номер в `dsp::FILTER_KINDS`, 0 — выключена) и частота среза, Гц.
type Trial = [(u32, u32); 2];
/// Спектры и задержки `gyroUnfilt` после пробного фильтра, по осям.
type TrialResult = ([Vec<[f64; 2]>; 3], [Option<f64>; 3]);

/// Результат анализа вместе с участком лога, для которого он посчитан.
type ForRange<T> = Option<((usize, usize), T)>;

struct Loaded {
    log: bbl::Log,
    sample_rate: f64,
    /// Разрывы записи во всём логе.
    gaps: dsp::Gaps,
    /// Сколько первых полей лога записано прошивкой; остальные дописаны для анализа.
    logged_fields: usize,
    axis_fields: Vec<AxisField>,
    /// Поля без разбивки по осям: `(столбец, включено)`.
    other_fields: Vec<(usize, bool)>,
    /// Участок лога, видимый на кривых (номера кадров); по нему считается весь анализ, кроме карт.
    range: (usize, usize),
    /// Просьба вернуть на кривых масштаб «весь лог».
    reset_zoom: bool,
    /// Измерительные курсоры A и B на кривых, время в секундах.
    cursors: [Option<f64>; 2],
    /// Просьба показать на кривых этот участок времени.
    zoom_to: Option<(f64, f64)>,
    spectra: ForRange<Spectra>,
    /// Задержка `gyroADC` относительно `gyroUnfilt` по осям, мс.
    delays: ForRange<[Option<f64>; 3]>,
    trial: ForRange<(Trial, TrialResult)>,
    maps: Option<Maps>,
    steps: ForRange<tabs::Steps>,
    split_steps: ForRange<(StepSplit, tabs::SplitSteps)>,
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
        let logged_fields = log.names.len();
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
            gaps: dsp::Gaps::find(&log.time, log.sample_rate()),
            logged_fields,
            sample_rate: log.sample_rate(),
            range: (0, log.time.len()),
            reset_zoom: false,
            cursors: [None; 2],
            zoom_to: None,
            axis_fields,
            other_fields,
            spectra: None,
            delays: None,
            trial: None,
            maps: None,
            steps: None,
            split_steps: None,
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

    /// Множитель из значения поля eRPM в частоту вращения мотора, Гц (об/с).
    /// В логе eRPM записан в сотнях электрических оборотов в минуту.
    fn erpm_to_hz(&self) -> f32 {
        let poles: f32 = self.log.header("motor_poles").and_then(|v| v.trim().parse().ok()).unwrap_or(14.0);
        100.0 / (poles / 2.0) / 60.0
    }

    /// Средняя по моторам частота вращения, Гц (об/с), из телеметрии eRPM.
    fn motor_hz(&self) -> Option<Vec<f32>> {
        let cols: Vec<&[f32]> = (0..8).filter_map(|i| self.log.col(&format!("eRPM[{i}]"))).collect();
        let to_hz = self.erpm_to_hz() / cols.len() as f32;
        (!cols.is_empty()).then(|| (0..self.log.time.len()).map(|i| cols.iter().map(|c| c[i]).sum::<f32>() * to_hz).collect())
    }

    /// Числа из параметра заголовка лога (`"250,500"` → `[250, 500]`).
    fn header_numbers(&self, name: &str) -> Vec<f64> {
        self.log.header(name).map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default()
    }

    /// Линии для карты шума вида `kind`: срезы фильтров из заголовка лога
    /// и гармоники оборотов моторов. Для D-составляющей берутся её фильтры,
    /// для остальных сигналов — фильтры гироскопа.
    fn overlays(&self, signal: &str, kind: MapKind, x_max: f64) -> Vec<Overlay> {
        let mut out = Vec::new();
        let f_max = self.sample_rate / 2.0;
        let stage = if signal == "axisD" { "dterm" } else { "gyro" };
        let mut horizontal = |label: String, hz: f64| {
            if hz > 0.0 && hz < f_max {
                out.push(Overlay { label, points: vec![[0.0, hz], [x_max, hz]], harmonic: false });
            }
        };
        for name in ["lpf1_static_hz", "lpf2_static_hz", "notch_hz"] {
            for hz in self.header_numbers(&format!("{stage}_{name}")) {
                horizontal(format!("{stage}_{name} {hz:.0}"), hz);
            }
        }
        if stage == "gyro" {
            for name in ["dyn_notch_min_hz", "dyn_notch_max_hz", "adrc_gyro_lpf_hz"] {
                for hz in self.header_numbers(name) {
                    horizontal(format!("{name} {hz:.0}"), hz);
                }
            }
        }

        // Усреднение сигнала по ~800 участкам времени: линия на спектрограмме не тяжелее самой карты.
        let over_time = |values: &[f32], to_hz: &dyn Fn(f64) -> f64| -> Vec<[f64; 2]> {
            let chunk = (values.len() / 800).max(1);
            values.chunks(chunk).zip(self.log.time.chunks(chunk)).map(|(v, t)| [t[t.len() / 2], to_hz(v.iter().sum::<f32>() as f64 / v.len() as f64)]).collect()
        };
        let throttle = self.throttle_pct();

        // Динамический фильтр нижних частот: срез растёт с газом.
        if let ([min, max], expo, false) = (&self.header_numbers(&format!("{stage}_lpf1_dyn_hz"))[..], self.header_numbers(&format!("{stage}_lpf1_dyn_expo")), matches!(kind, MapKind::Rpm))
            && *min > 0.0
            && max > min
        {
            let cutoff = |pct: f64| dsp::dyn_lpf_hz((pct / 100.0).clamp(0.0, 1.0), *min, *max, expo.first().copied().unwrap_or(0.0));
            let points = match (kind, &throttle) {
                (MapKind::Throttle, _) => (0..=100).map(|pct| [pct as f64, cutoff(pct as f64)]).collect(),
                (_, Some(throttle)) => over_time(throttle, &cutoff),
                _ => Vec::new(),
            };
            out.push(Overlay { label: format!("{stage}_lpf1_dyn_hz {min:.0}–{max:.0}"), points, harmonic: false });
        }

        // Гармоники оборотов моторов.
        if let Some(motor_hz) = self.motor_hz() {
            for k in 1..=3u32 {
                let harmonic = |hz: f64| hz * k as f64;
                let points: Vec<[f64; 2]> = match (kind, &throttle) {
                    (MapKind::Rpm, _) => {
                        let end = x_max.min(f_max / k as f64);
                        vec![[0.0, 0.0], [end, harmonic(end)]]
                    }
                    (MapKind::Time, _) => over_time(&motor_hz, &harmonic),
                    (MapKind::Throttle, Some(throttle)) => {
                        // Средние обороты на каждом проценте газа.
                        let mut bins = [(0f64, 0u32); 101];
                        for (pct, hz) in throttle.iter().zip(&motor_hz) {
                            let bin = &mut bins[(*pct as usize).min(100)];
                            *bin = (bin.0 + *hz as f64, bin.1 + 1);
                        }
                        // Проценты газа, на которых дрон почти не бывал, пропускаются: среднее по горстке кадров — шум.
                        let enough = (motor_hz.len() / 500).max(20) as u32;
                        bins.iter().enumerate().filter(|(_, b)| b.1 >= enough).map(|(pct, b)| [pct as f64, harmonic(b.0 / b.1 as f64)]).collect()
                    }
                    _ => Vec::new(),
                };
                out.push(Overlay { label: tf!("{k}× оборотов", "{k}× RPM"), points, harmonic: true });
            }
        }
        // Выше верхней частоты карты линии не рисуются.
        for overlay in &mut out {
            overlay.points.iter_mut().filter(|p| p[1] >= f_max).for_each(|p| p[1] = f64::NAN);
        }
        out.retain(|o| o.points.iter().filter(|p| !p[1].is_nan()).count() >= 2);
        out
    }

    fn spectra(&mut self) -> &Spectra {
        if self.spectra.as_ref().is_none_or(|(range, _)| *range != self.range) {
            let (lo, hi) = self.range;
            let spectra = SPECTRUM_BASES
                .iter()
                .filter(|&&base| self.axis_fields.iter().any(|f| f.base == base))
                .map(|&base| (base, self.axis_cols(base).map(|c| c.map_or(Vec::new(), |c| dsp::welch_db_gaps(&c[lo..hi], self.sample_rate, &self.gaps.within(lo, hi))))))
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

    /// Ступени lowpass 1 и 2 гироскопа из настроек лога; `None`, если в логе их нет.
    /// У динамического lowpass 1 берётся нижняя частота — худший случай по задержке.
    fn trial_from_log(&self) -> Option<Trial> {
        // Номера видов фильтра в заголовке лога: 0 — PT1, 1 — BIQUAD, 2 — PT2, 3 — PT3.
        let kind = |name: &str| [1, 4, 2, 3].get(self.header_numbers(name).first().copied().unwrap_or(0.0) as usize).copied().unwrap_or(1);
        let hz = |names: &[&str]| names.iter().filter_map(|n| self.header_numbers(n).first().copied()).find(|hz| *hz > 0.0);
        let stage = |hz: Option<f64>, kind: u32| hz.map_or((0, 100), |hz| (kind, hz as u32));
        let (first, second) = (hz(&["gyro_lpf1_dyn_hz", "gyro_lpf1_static_hz"]), hz(&["gyro_lpf2_static_hz"]));
        (first.is_some() || second.is_some()).then(|| [stage(first, kind("gyro_lpf1_type")), stage(second, kind("gyro_lpf2_type"))])
    }

    fn trial(&mut self, trial: Trial) -> &TrialResult {
        if self.trial.as_ref().is_none_or(|(range, (t, _))| *range != self.range || *t != trial) {
            let ((lo, hi), fs) = (self.range, self.sample_rate);
            // Сигнал проходит обе ступени подряд, как lowpass 1 и lowpass 2 гироскопа в прошивке.
            let chain = |x: &[f32]| trial.iter().fold(x.to_vec(), |x, &(kind, hz)| dsp::apply_filter(&x, kind, hz as f64, fs));
            let filtered = self.axis_cols("gyroUnfilt").map(|c| c.map(|c| (&c[lo..hi], chain(&c[lo..hi]))));
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
            let binned = |key: &Option<Vec<f32>>| cols.iter().map(|c| dsp::binned_heatmap_gaps((*c)?, key.as_deref()?, fs, &self.gaps)).collect();
            let set = |grids: Vec<Option<dsp::Grid>>, x_max: f64, kind: MapKind| {
                // Верх шкалы — не максимум, а уровень, выше которого лишь 0.5 % ячеек:
                // иначе один удар или краш в логе затемняет всю остальную карту.
                let mut levels: Vec<f32> = grids.iter().flatten().flat_map(|g| &g.db).copied().filter(|v| !v.is_nan()).collect();
                let top_db = if levels.is_empty() {
                    0.0
                } else {
                    let k = levels.len() * 995 / 1000;
                    *levels.select_nth_unstable_by(k, f32::total_cmp).1
                };
                MapSet { grids, top_db, x_max, overlays: self.overlays(signal, kind, x_max), textures: Vec::new() }
            };
            let sets = [
                set(binned(&throttle), 100.0, MapKind::Throttle),
                set(cols.iter().map(|c| dsp::spectrogram((*c)?, &self.log.time, fs)).collect(), *self.log.time.last().unwrap(), MapKind::Time),
                set(binned(&rpm_pct), rpm_max as f64, MapKind::Rpm),
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
    // По Y наоборот реже: на высоком окне подписи через каждые 10 Гц сливались в столбик.
    let y_axis = AxisHints::new_y().label_spacing(egui::Rangef::new(45.0, 90.0));
    Plot::new(id).custom_x_axes(vec![x_axis]).custom_y_axes(vec![y_axis]).y_axis_min_width(Y_AXIS_WIDTH).grid_spacing(egui::Rangef::new(16.0, 60.0)).grid_color(theme::MUTED)
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
    // По возрастанию частоты; параметры с одной и той же частотой — одной подписью.
    marks.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut merged: Vec<(String, f64)> = Vec::new();
    for (name, hz) in marks {
        match merged.last_mut().filter(|last| last.1 == hz) {
            Some(last) => last.0 = format!("{}, {name}", last.0),
            None => merged.push((name, hz)),
        }
    }
    merged
}

/// Цветная метка серии: кружок цвета линии; подпись рядом пишется обычным цветом текста.
fn swatch(ui: &mut egui::Ui, name: &str) {
    swatch_color(ui, theme::series_color(name));
}

fn swatch_color(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

fn line<'a>(name: &str, points: Vec<[f64; 2]>) -> Line<'a> {
    Line::new(name, PlotPoints::new(points)).color(theme::series_color(name)).width(1.5)
}

/// Множитель и единица измерения для полей, которые в логе записаны в сотых долях.
fn unit(name: &str) -> (f64, &'static str) {
    match name {
        "vbatLatest" => (0.01, t!(", В", ", V")),
        "amperageLatest" => (0.01, t!(", А", ", A")),
        _ => (1.0, ""),
    }
}

/// «Roll 1.9 мс, Pitch 2.0 мс, Yaw нет данных».
fn per_axis_ms(values: [Option<f64>; 3]) -> String {
    let text = |a: usize| values[a].map_or(tf!("{} нет данных", "{} no data", AXES[a]), |ms| tf!("{} {ms:.1} мс", "{} {ms:.1} ms", AXES[a]));
    [text(0), text(1), text(2)].join(", ")
}

fn show_curves(ui: &mut egui::Ui, loaded: &mut Loaded) {
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(
            theme::MUTED,
            t!(
                "Масштаб — Ctrl + колесо или рамка правой кнопкой, двойной щелчок — весь лог. Видимый участок анализируется на остальных вкладках. Щелчок ставит курсор A, Shift + щелчок — курсор B.",
                "Zoom with Ctrl + wheel or a right-button box; double-click shows the whole log. The visible range is what the other tabs analyse. Click sets cursor A, Shift + click sets cursor B."
            ),
        );
    });
    if loaded.cursors.iter().any(Option::is_some) {
        ui.horizontal(|ui| {
            let mark = |name: &str, at: Option<f64>| at.map_or(String::new(), |t| tf!("{} {:.3} с", "{} {:.3} s", name, t));
            ui.label(format!("{}  {}", mark("A", loaded.cursors[0]), mark("B", loaded.cursors[1])));
            if let [Some(a), Some(b)] = loaded.cursors {
                let delta = (b - a).abs();
                // Интервал между курсорами как период: удобно мерить частоту раскачки по двум соседним гребням.
                ui.label(tf!("Δ {:.1} мс, как период — {:.1} Гц", "Δ {:.1} ms, as a period — {:.1} Hz", delta * 1000.0, 1.0 / delta.max(1e-6)));
                if ui.button(t!("Показать участок A–B", "Show range A–B")).clicked() {
                    loaded.zoom_to = Some((a.min(b), a.max(b)));
                }
            }
            if ui.button(t!("Убрать курсоры", "Clear cursors")).clicked() {
                loaded.cursors = [None; 2];
            }
        });
    }

    let log = &loaded.log;
    let others: Vec<usize> = loaded.other_fields.iter().filter(|(_, on)| *on).map(|(i, _)| *i).collect();
    let rows = 3 + !others.is_empty() as usize;
    let height = row_height(ui, rows);
    let (mut range, mut cursors) = (loaded.range, loaded.cursors);
    for row in 0..rows {
        let mut plot = new_plot(("curves", row)).height(height).link_axis("curves", [true, false]).link_cursor("curves", [true, false]).y_axis_label(AXES.get(row).copied().unwrap_or(t!("Прочее", "Other")));
        if row == rows - 1 {
            plot = plot.x_axis_label(t!("время, с", "time, s"));
        }
        plot.show(ui, |plot| {
            if loaded.reset_zoom {
                plot.set_auto_bounds([true, true]);
            }
            if let Some((from, to)) = loaded.zoom_to.filter(|(from, to)| to > from) {
                plot.set_plot_bounds_x(from..=to);
            }
            if plot.response().clicked()
                && let Some(point) = plot.pointer_coordinate()
            {
                cursors[plot.ctx().input(|i| i.modifiers.shift) as usize] = Some(point.x);
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
                plot.vline(VLine::new(event_text(event), *time).color(theme::MUTED).style(LineStyle::dashed_dense()).width(1.0));
            }
            for (name, at) in ["A", "B"].into_iter().zip(cursors) {
                if let Some(at) = at {
                    plot.vline(VLine::new(name, at).color(theme::ACCENT).width(1.5));
                    plot.text(Text::new(name, PlotPoint::new(at, plot.plot_bounds().max()[1]), format!(" {name}")).color(theme::ACCENT).anchor(egui::Align2::LEFT_TOP));
                }
            }
        });
    }
    (loaded.range, loaded.cursors) = (range, cursors);
    (loaded.reset_zoom, loaded.zoom_to) = (false, None);
}

/// Список полей без разбивки по осям: поля с общим именем (`debug[0]`…`debug[7]`)
/// свёрнуты в группу; при поиске показываются подходящие поля россыпью.
fn other_fields_list(ui: &mut egui::Ui, loaded: &mut Loaded, filter: &str) {
    let base = |name: &str| name.split('[').next().unwrap_or(name).to_string();
    let names: Vec<String> = loaded.other_fields.iter().map(|(i, _)| loaded.log.names[*i].clone()).collect();
    let field = |ui: &mut egui::Ui, name: &str, on: &mut bool| {
        ui.horizontal(|ui| {
            swatch(ui, name);
            ui.checkbox(on, name);
        });
    };
    if !filter.is_empty() {
        for (k, name) in names.iter().enumerate().filter(|(_, n)| n.to_lowercase().contains(&filter.to_lowercase())) {
            field(ui, name, &mut loaded.other_fields[k].1);
        }
        return;
    }
    let mut k = 0;
    while k < names.len() {
        let group = names[k..].iter().take_while(|n| base(n) == base(&names[k])).count();
        if group == 1 {
            field(ui, &names[k], &mut loaded.other_fields[k].1);
        } else {
            let chosen = loaded.other_fields[k..k + group].iter().filter(|f| f.1).count();
            let title = if chosen > 0 { format!("{} ({chosen}/{group})", base(&names[k])) } else { format!("{} ({group})", base(&names[k])) };
            egui::CollapsingHeader::new(title).id_salt(("fields", base(&names[k]))).show(ui, |ui| {
                for j in k..k + group {
                    field(ui, &names[j], &mut loaded.other_fields[j].1);
                }
            });
        }
        k += group;
    }
}

fn show_spectrum(ui: &mut egui::Ui, loaded: &mut Loaded, compare: Option<&mut Loaded>, view: &mut View) {
    let (lo, hi) = loaded.range;
    let span = tf!("Участок {:.2}–{:.2} с. Чтобы выбрать другой, измените масштаб на вкладке «Кривые».", "Range {:.2}–{:.2} s. To pick another one, zoom on the Curves tab.", loaded.log.time[lo], loaded.log.time[hi.max(lo + 1) - 1]);
    let nyquist = loaded.sample_rate / 2.0;
    let marks: Vec<(String, f64)> = filter_marks(&loaded.log).into_iter().filter(|(_, hz)| *hz < nyquist).collect();
    let delays = loaded.delays();
    let has_raw = loaded.axis_fields.iter().any(|f| f.base == "gyroUnfilt");
    let loaded_trial = loaded.trial_from_log();
    let trial = (view.trial_on && has_raw).then(|| loaded.trial(view.trial).clone());
    let spectra = loaded.spectra();
    let compared = compare.map(|c| c.spectra());

    ui.horizontal_wrapped(|ui| {
        for (base, _) in spectra {
            swatch(ui, base);
            ui.label(*base);
        }
        if trial.is_some() {
            swatch(ui, TRIAL_FILTER);
            ui.label(trial_filter_label());
        }
        ui.separator();
        ui.colored_label(theme::MUTED, span);
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(t!("Задержка фильтров гироскопа", "Gyro filter delay"));
        ui.colored_label(theme::MUTED, if has_raw { per_axis_ms(delays) } else { t!("нет поля gyroUnfilt в логе", "no gyroUnfilt field in the log").into() });
        if has_raw {
            ui.separator();
            ui.checkbox(&mut view.trial_on, t!("Пробный фильтр на gyroUnfilt", "Trial filter on gyroUnfilt"));
            if view.trial_on {
                // Две ступени подряд — как lowpass 1 и lowpass 2 гироскопа.
                for (stage, (kind, hz)) in view.trial.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("trial_kind", stage)).width(70.0).selected_text(dsp::FILTER_KINDS[*kind as usize]).show_ui(ui, |ui| {
                        for (k, name) in dsp::FILTER_KINDS.iter().enumerate() {
                            ui.selectable_value(kind, k as u32, *name);
                        }
                    });
                    if *kind != 0 {
                        ui.add(egui::Slider::new(hz, 20..=(nyquist as u32).max(21)).suffix(t!(" Гц", " Hz")));
                    }
                }
                if ui.button(t!("Как в логе", "As in the log")).on_hover_text(t!("Взять виды и частоты lowpass 1 и 2 гироскопа из настроек лога. Сравните кривую с gyroADC: разница — работа notch- и RPM-фильтров.", "Take the gyro lowpass 1 and 2 types and cutoffs from the log settings. Compare the curve with gyroADC: the difference is the work of the notch and RPM filters.")).clicked() {
                    view.trial = loaded_trial.unwrap_or(view.trial);
                }
                if let Some((_, trial_delays)) = &trial {
                    ui.colored_label(theme::MUTED, tf!("задержка с ним: {}", "delay with it: {}", per_axis_ms(*trial_delays)));
                }
            }
        }
    });

    let height = row_height(ui, 3);
    for (a, axis) in AXES.iter().enumerate() {
        let mut plot = new_plot(("spectrum", a)).height(height).link_axis("spectrum", [true, true]).link_cursor("spectrum", [true, false]).y_axis_label(tf!("{axis}, дБ", "{axis}, dB"));
        if a == 2 {
            plot = plot.x_axis_label(t!("частота, Гц", "frequency, Hz"));
        }
        plot.show(ui, |plot| {
            for (base, by_axis) in spectra {
                plot.line(line(base, by_axis[a].clone()));
            }
            if let Some((trial_spectra, _)) = &trial {
                plot.line(line(TRIAL_FILTER, trial_spectra[a].clone()).name(trial_filter_label()));
            }
            for (base, by_axis) in compared.into_iter().flatten() {
                plot.line(line(base, by_axis[a].clone()).name(tf!("{base}, сравнение", "{base}, comparison")).style(LineStyle::dashed_loose()));
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
                    // Длинный список параметров на одной частоте сокращается до первого и счётчика.
                    let short = match name.split_once(", ") {
                        Some((first, rest)) => format!("{first} +{}", rest.split(", ").count()),
                        None => name.clone(),
                    };
                    let label = Text::new(name, PlotPoint::new(*hz, top - step * (k % 4) as f64), format!(" {short} {hz:.0}"));
                    plot.text(label.color(theme::MUTED).anchor(egui::Align2::LEFT_TOP));
                }
            }
        });
    }
}

/// Шкала цвета карт: полоска палитры с подписями крайних уровней.
fn colorbar(ui: &mut egui::Ui, top_db: f32, range_db: f32) {
    ui.colored_label(theme::MUTED, tf!("{:.0} дБ", "{:.0} dB", top_db - range_db));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 10.0), egui::Sense::hover());
    let steps = 64;
    for i in 0..steps {
        let x = |k: usize| rect.left() + rect.width() * k as f32 / steps as f32;
        let cell = egui::Rect::from_min_max(egui::pos2(x(i), rect.top()), egui::pos2(x(i + 1), rect.bottom()));
        ui.painter().rect_filled(cell, 0.0, theme::colormap(i as f32 / (steps - 1) as f32));
    }
    ui.colored_label(theme::MUTED, tf!("{top_db:.0} дБ", "{top_db:.0} dB"));
}

/// Подпись под курсором на карте: положение по осям и уровень в ячейке матрицы.
fn map_readout(grid: Option<&dsp::Grid>, unit: &str, x_max: f64, f_max: f64, x: f64, hz: f64) -> String {
    let level = grid.and_then(|g| {
        let (col, row) = (x / x_max * g.w as f64, hz / f_max * (g.h - 1) as f64);
        ((0.0..g.w as f64).contains(&col) && (0.0..g.h as f64).contains(&row)).then(|| g.db[col as usize * g.h + row as usize])
    });
    match level.filter(|db| !db.is_nan()) {
        Some(db) => tf!("{x:.1} {unit}, {hz:.0} Гц: {db:.0} дБ", "{x:.1} {unit}, {hz:.0} Hz: {db:.0} dB"),
        None => tf!("{x:.1} {unit}, {hz:.0} Гц: нет данных", "{x:.1} {unit}, {hz:.0} Hz: no data"),
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
    let x_label = [t!("газ, %", "throttle, %"), t!("время, с", "time, s"), t!("обороты моторов, Гц", "motor speed, Hz")][kind as usize];
    let overlays = view.overlays;

    ui.horizontal(|ui| {
        if !spectrogram && has_rpm {
            ui.selectable_value(&mut view.by_rpm, false, t!("По газу", "By throttle"));
            ui.selectable_value(&mut view.by_rpm, true, t!("По оборотам", "By RPM"));
            ui.separator();
        }
        ui.label(t!("Сигнал", "Signal"));
        egui::ComboBox::from_id_salt("map_signal").selected_text(view.map_signal).show_ui(ui, |ui| {
            for base in bases {
                ui.selectable_value(&mut view.map_signal, base, base);
            }
        });
        ui.separator();
        ui.label(t!("Диапазон", "Range"));
        ui.add(egui::Slider::new(&mut view.map_range_db, 20.0..=100.0).suffix(t!(" дБ", " dB")).fixed_decimals(0));
        ui.separator();
        colorbar(ui, set.top_db, view.map_range_db);
        ui.separator();
        ui.checkbox(&mut view.overlays, t!("Фильтры и обороты", "Filters and RPM")).on_hover_text(
            t!("Штриховые линии — частоты среза фильтров из настроек лога: шум выше линии фильтр давит. Пунктирные 1×, 2×, 3× — где лежит шум, кратный оборотам моторов (нужна телеметрия оборотов). Яркая полоса вдоль пунктира — моторы и пропеллеры; полоса на постоянной частоте — резонанс рамы.", "Dashed lines are filter cutoff frequencies from the log settings: the filter suppresses noise above the line. Dotted 1×, 2×, 3× lines show where noise at multiples of motor speed lies (needs RPM telemetry). A bright band along a dotted line means motors and props; a band at a constant frequency means a frame resonance."),
        );
    });

    let plot_axis = |ui: &mut egui::Ui, a: usize, height: f32, with_x_label: bool| {
        let Some(texture) = set.textures.get(a).and_then(Option::as_ref) else {
            ui.colored_label(theme::MUTED, tf!("{}: в логе нет этого сигнала, газа или оборотов", "{}: the log has no such signal, throttle or RPM", AXES[a]));
            return;
        };
        let mut plot = new_plot(("map", kind as usize, a))
            .height(height)
            .link_axis(egui::Id::new(("map", kind as usize)), [true, true])
            .link_cursor(egui::Id::new(("map", kind as usize)), [true, true])
            .set_margin_fraction(egui::Vec2::ZERO)
            .show_grid(false)
            .y_axis_label(tf!("{}, Гц", "{}, Hz", AXES[a]));
        if with_x_label {
            plot = plot.x_axis_label(x_label);
        }
        // В углу — что под курсором: положение по осям и уровень в этой ячейке.
        let grid = set.grids[a].as_ref();
        let unit = ["%", t!("с", "s"), t!("Гц", "Hz")][kind as usize];
        let readout = move |point: &PlotPoint, _: &egui_plot::PlotBounds| map_readout(grid, unit, set.x_max, f_max, point.x, point.y);
        plot = plot.coordinates_formatter(egui_plot::Corner::RightTop, egui_plot::CoordinatesFormatter::new(readout));
        plot.show(ui, |plot| {
            let center = PlotPoint::new(set.x_max / 2.0, f_max / 2.0);
            plot.image(PlotImage::new(AXES[a], texture.id(), center, [set.x_max as f32, f_max as f32]));
            if overlays {
                for overlay in &set.overlays {
                    let style = if overlay.harmonic { LineStyle::Dotted { spacing: 6.0 } } else { LineStyle::dashed_dense() };
                    // Линия рисуется кусками между разрывами (`NaN`).
                    for piece in overlay.points.split(|p| p[1].is_nan()).filter(|piece| piece.len() >= 2) {
                        plot.line(Line::new(&overlay.label, PlotPoints::new(piece.to_vec())).color(theme::INK).style(style).width(1.0));
                    }
                    // Подпись — у последней видимой точки в правой части линии, чтобы не упиралась в край.
                    let visible: Vec<&[f64; 2]> = overlay.points.iter().filter(|p| !p[1].is_nan()).collect();
                    let [x, hz] = *visible[visible.len() * 9 / 10];
                    // У левого края подпись идёт вправо от точки, иначе её обрезало бы.
                    let anchor = if x < set.x_max * 0.3 { egui::Align2::LEFT_BOTTOM } else { egui::Align2::RIGHT_BOTTOM };
                    plot.text(Text::new(&overlay.label, PlotPoint::new(x, hz), &overlay.label).color(theme::INK).anchor(anchor));
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
    /// Линии фильтров и гармоник оборотов поверх карт шума.
    overlays: bool,
    trial_on: bool,
    trial: Trial,
    /// Деление отклика на вкладке «Отклик».
    step_split: StepSplit,
    /// Поиск по списку полей на вкладке «Кривые».
    field_filter: String,
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
    /// Заголовок окна, выставленный последним.
    title: String,
    tab: Tab,
    view: View,
    /// Режим «снять и выйти»: через сколько кадров сохранить снимок вкладки и закрыть окно.
    snapshot_in: Option<u32>,
}

/// Все логи с данными из файла.
fn load(path: &Path) -> Result<Vec<Loaded>, String> {
    let data = std::fs::read(path).map_err(|e| tf!("Не удалось прочитать {}: {e}", "Could not read {}: {e}", path.display()))?;
    let mut errors = vec![tf!("В файле {} нет логов с данными", "No logs with data in {}", path.display())];
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
        let slug = TABS.iter().find(|t| t.0 == self.tab).map_or("", |t| t.3);
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let target = self.path.with_file_name(format!("{stem}_{slug}.png"));
        let saved = image::save_buffer(&target, image.as_raw(), image.width() as u32, image.height() as u32, image::ColorType::Rgba8);
        match saved {
            Ok(()) => self.notice = Some(tf!("Снимок сохранён: {}", "Snapshot saved: {}", target.display())),
            Err(e) => self.error = Some(tf!("Не удалось сохранить снимок в {}: {e}", "Could not save the snapshot to {}: {e}", target.display())),
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
        let view = View { map_signal: "gyroADC", map_range_db: 50.0, by_rpm: false, overlays: true, trial_on: false, trial: [(1, 100), (0, 200)], step_split: StepSplit::None, field_filter: String::new(), settings_filter: String::new(), only_different: false };
        Self { path: PathBuf::new(), logs: Vec::new(), compare: None, current: 0, error: None, notice: None, title: String::new(), tab: Tab::Overview, view, snapshot_in: None }
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
        // Имя открытого лога — в заголовке окна: видно на панели задач и при нескольких окнах.
        let title = if self.logs.is_empty() { "FPV Log Analyzer".to_string() } else { format!("{} — FPV Log Analyzer", file_name(&self.path)) };
        if self.title != title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if let Some(image) = screenshot {
            self.save_screenshot(&image);
            if self.snapshot_in.is_some() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if self.snapshot_in.is_some() {
            // На снимке для документации не должно быть перекрестья и подсказок от мыши,
            // случайно оказавшейся над окном.
            ui.input_mut(|i| i.pointer = Default::default());
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
                if ui.button(t!("Открыть лог…", "Open log…")).clicked()
                    && let Some(path) = pick_log()
                {
                    self.open(&path);
                }
                if !self.logs.is_empty() {
                    if self.compare.is_some() {
                        if ui.button(t!("Убрать сравнение", "Remove comparison")).clicked() {
                            self.compare = None;
                        }
                    } else if ui.button(t!("Сравнить с…", "Compare with…")).clicked()
                        && let Some(path) = pick_log()
                    {
                        self.open_compare(&path);
                    }
                    if ui.button(t!("Сохранить снимок", "Save snapshot")).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    }
                }
                if self.logs.len() > 1 {
                    egui::ComboBox::from_id_salt("log").selected_text(tf!("Запись {} из {}", "Record {} of {}", self.current + 1, self.logs.len())).show_ui(ui, |ui| {
                        for (i, l) in self.logs.iter().enumerate() {
                            ui.selectable_value(&mut self.current, i, tf!("Запись {}, {:.0} с", "Record {}, {:.0} s", i + 1, l.log.time.last().unwrap_or(&0.0)));
                        }
                    });
                }
                ui.separator();
                for tab in &TABS {
                    ui.selectable_value(&mut self.tab, tab.0, tab_name(tab));
                }
                ui.separator();
                // Переключатель языка. Тексты находок и подписи линий посчитаны заранее —
                // при смене языка они сбрасываются и собираются заново.
                let english = i18n::english();
                if ui.selectable_label(false, if english { "Русский" } else { "English" }).on_hover_text(if english { "Переключить интерфейс на русский" } else { "Switch the interface to English" }).clicked() {
                    i18n::set_english(!english);
                    for loaded in self.logs.iter_mut().chain(self.compare.iter_mut().map(|(_, c)| c)) {
                        (loaded.overview, loaded.split_steps, loaded.maps) = (None, None, None);
                    }
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
                    fact(ui, t!("Лог", "Log"), file_name(&self.path));
                    fact(ui, t!("Прошивка", "Firmware"), l.log.header("Firmware revision").unwrap_or(t!("неизвестна", "unknown")).to_string());
                    fact(ui, t!("Запись", "Rate"), tf!("{:.0} Гц", "{:.0} Hz", l.sample_rate));
                    fact(ui, t!("Длительность", "Duration"), tf!("{:.1} с", "{:.1} s", l.log.time.last().unwrap_or(&0.0)));
                    fact(ui, t!("Разрывов записи", "Recording gaps"), l.log.corrupt.to_string());
                    if l.range != (0, l.log.time.len()) {
                        let (lo, hi) = l.range;
                        fact(ui, t!("Анализируется участок", "Analysing range"), tf!("{:.2}–{:.2} с", "{:.2}–{:.2} s", l.log.time[lo], l.log.time[hi.max(lo + 1) - 1]));
                        whole_log = ui.button(t!("Весь лог", "Whole log")).clicked();
                    }
                    if let Some((name, _)) = &self.compare {
                        fact(ui, t!("Сравнение, штриховые линии", "Comparison (dashed lines)"), name.clone());
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
                ui.centered_and_justified(|ui| ui.colored_label(theme::MUTED, t!("Перетащите сюда файл лога (.bbl) или нажмите «Открыть лог…»", "Drop a log file (.bbl) here or click “Open log…”")));
            });
            return;
        };

        if self.tab == Tab::Curves {
            egui::Panel::left("fields").show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.colored_label(theme::MUTED, t!("По осям", "Per axis"));
                    for f in &mut loaded.axis_fields {
                        ui.horizontal(|ui| {
                            swatch(ui, f.base);
                            ui.checkbox(&mut f.on, f.base);
                        });
                    }
                    ui.add_space(8.0);
                    ui.colored_label(theme::MUTED, t!("Прочее", "Other"));
                    ui.add(egui::TextEdit::singleline(&mut self.view.field_filter).hint_text(t!("поиск поля", "find a field")).desired_width(150.0));
                    other_fields_list(ui, loaded, &self.view.field_filter);
                });
            });
        }

        let compare = self.compare.as_mut().map(|(_, c)| c);
        let view = &mut self.view;
        let mut go_to = None;
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Overview => go_to = tabs::show_overview(ui, loaded),
            Tab::Curves => show_curves(ui, loaded),
            Tab::Spectrum => show_spectrum(ui, loaded, compare, view),
            Tab::Heatmap => show_maps(ui, loaded, false, view),
            Tab::Spectrogram => show_maps(ui, loaded, true, view),
            Tab::Step => tabs::show_step(ui, loaded, compare, view),
            Tab::Error => tabs::show_error(ui, loaded),
            Tab::Motors => tabs::show_motors(ui, loaded),
            Tab::Settings => tabs::show_settings(ui, loaded, compare.map(|c| &*c), view),
        });
        if let Some(tab) = go_to {
            self.tab = tab;
        }
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

/// Печатает запись в CSV: время от начала в микросекундах и первые `fields` полей —
/// те, что записаны в логе (расчётные поля анализа в выгрузку не идут).
fn write_csv(log: &bbl::Log, fields: usize) {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    // Ошибка записи значит, что читатель закрыл поток (`| head`) — молча прекращаем.
    let mut line = |text: String| writeln!(out, "{text}").is_ok();
    if !line(format!("time_us,{}", log.names[..fields].join(","))) {
        return;
    }
    for (row, time) in log.time.iter().enumerate() {
        let values: Vec<String> = log.cols[..fields].iter().map(|c| c[row].to_string()).collect();
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
        (mode, app.snapshot_in) = (Some(tab.to_string()), Some(60)); // с запасом на разворачивание окна
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
            Some(loaded) => write_csv(&loaded.log, loaded.logged_fields),
            None => eprintln!("{}", app.error.unwrap_or(tf!("В файле нет записи {index}", "The file has no record {index}"))),
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
            let (duration, frames) = (*loaded.log.time.last().unwrap_or(&0.0), loaded.log.time.len());
            say(format_args!("{}", tf!("Запись {}: {:.0} с, {:.0} Гц, кадров {}, разрывов записи {}", "Record {}: {:.0} s, {:.0} Hz, {} frames, {} recording gaps", i + 1, duration, loaded.sample_rate, frames, loaded.log.corrupt)));
            for finding in &tabs::overview(loaded).findings {
                say(format_args!("  [{}] {}", if finding.important { t!("важно", "important") } else { t!("заметка", "note") }, finding.text));
            }
            for (time, event) in &loaded.log.events {
                say(format_args!("  [{}] {time:.2} {}: {}", t!("событие", "event"), t!("с", "s"), event_text(event)));
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
        Some("step-rate") => (app.tab, app.view.step_split) = (Tab::Step, StepSplit::Rate),
        Some("step-throttle") => (app.tab, app.view.step_split) = (Tab::Step, StepSplit::Throttle),
        mode => app.tab = TABS.iter().find(|t| Some(t.3) == mode).map_or(app.tab, |t| t.0),
    }
    if let Some(path) = args.next() {
        app.open_compare(Path::new(&path));
    }
    // Приложение лежит в общей ячейке: если первая попытка открыть окно не удалась
    // до его создания, вторая заберёт его оттуда же.
    let snapshot = app.snapshot_in.is_some();
    let slot = std::rc::Rc::new(std::cell::RefCell::new(Some(app)));
    let run = |renderer| {
        let slot = slot.clone();
        // Снимок для документации делается с окна, развёрнутого на весь экран.
        let viewport = egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]).with_maximized(snapshot);
        let options = eframe::NativeOptions { viewport, renderer, ..Default::default() };
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
        i18n::set_english(false); // тесты ищут элементы по русским подписям
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
        for english in [false, true] {
            i18n::set_english(english);
            harness.run_steps(2);
            for tab in &TABS {
                harness.get_by_label(tab_name(tab)).click();
                harness.run_steps(4);
                assert!(harness.state().tab == tab.0, "вкладка «{}» не открылась", tab_name(tab));
            }
        }
        i18n::set_english(false);
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

    /// Крайние случаи: участок из одного-двух кадров, из горстки кадров и лог для сравнения —
    /// ни одна вкладка не должна падать.
    #[test]
    fn tiny_ranges_and_comparison_do_not_break_tabs() {
        let Some(mut harness) = harness() else { return };
        let frames = harness.state().logs[0].log.time.len();
        let path = harness.state().path.clone();
        harness.state_mut().open_compare(&path);
        for range in [(0, 1), (0, 2), (0, 50), (frames - 1, frames), (frames / 2, frames / 2 + 300)] {
            for split in [StepSplit::None, StepSplit::Rate, StepSplit::Throttle] {
                for (tab, ..) in TABS.into_iter().filter(|t| t.0 != Tab::Curves) {
                    let app = harness.state_mut();
                    (app.tab, app.view.step_split, app.view.trial_on, app.view.by_rpm) = (tab, split, true, true);
                    app.logs[0].range = range;
                    harness.run_steps(2);
                }
            }
        }
    }

    #[test]
    fn cursors_measure_interval_and_zoom_to_it() {
        let Some(mut harness) = harness() else { return };
        harness.state_mut().tab = Tab::Curves;
        harness.state_mut().logs[0].cursors = [Some(10.0), Some(10.05)];
        harness.run_steps(3);
        // 50 мс между курсорами — период колебания 20 Гц.
        assert!(harness.query_by_label_contains("20.0 Гц").is_some());
        harness.get_by_label("Показать участок A–B").click();
        harness.run_steps(4);
        let log = &harness.state().logs[0];
        let (from, to) = (log.log.time[log.range.0], log.log.time[log.range.1 - 1]);
        assert!((from - 10.0).abs() < 0.01 && (to - 10.05).abs() < 0.01, "участок {from}–{to}");
        harness.get_by_label("Убрать курсоры").click();
        harness.run_steps(2);
        assert!(harness.query_by_label("Убрать курсоры").is_none());
    }

    #[test]
    fn map_readout_reports_cell_level() {
        i18n::set_english(false);
        let grid = dsp::Grid { w: 2, h: 3, db: vec![1.0, 2.0, 3.0, f32::NAN, 5.0, 6.0] };
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 25.0, 250.0), "25.0 %, 250 Гц: 2 дБ");
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 75.0, 10.0), "75.0 %, 10 Гц: нет данных");
        assert_eq!(map_readout(Some(&grid), "%", 100.0, 500.0, 150.0, 10.0), "150.0 %, 10 Гц: нет данных");
    }
}

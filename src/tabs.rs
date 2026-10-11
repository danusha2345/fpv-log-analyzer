//! Вкладки «Обзор», «Отклик», «Ошибка», «Моторы» и «Настройки».

use eframe::egui;
use egui_plot::{HLine, LineStyle};

use crate::{AXES, Loaded, View, dsp, line, new_plot, row_height, swatch, theme};

pub type Steps = [Option<dsp::StepResponse>; 3];

/// Печатает результаты анализа первой записи таблицей — для сверки с другими
/// программами: `step,ось,мс,отклик`; `bode,ось,Гц,дБ,градусы`;
/// `delay,ось,мс` (задержка фильтров гироскопа); `psd,сигнал,ось,Гц,дБ`.
pub fn print_response(loaded: &mut Loaded) {
    for (axis, delay) in AXES.iter().zip(loaded.delays()) {
        if let Some(ms) = delay {
            println!("delay,{axis},{ms:.3}");
        }
    }
    for (base, by_axis) in loaded.spectra().clone() {
        for (axis, spectrum) in AXES.iter().zip(&by_axis) {
            for p in spectrum {
                println!("psd,{base},{axis},{:.3},{:.3}", p[0], p[1]);
            }
        }
    }
    ensure_steps(loaded);
    for (axis, step) in AXES.iter().zip(&loaded.steps.as_ref().unwrap().1) {
        for p in step.iter().flat_map(|s| &s.points) {
            println!("step,{axis},{:.2},{:.4}", p[0], p[1]);
        }
        for p in step.iter().flat_map(|s| &s.bode) {
            println!("bode,{axis},{:.3},{:.3},{:.2}", p[0], p[1], p[2]);
        }
    }
}

fn ensure_steps(loaded: &mut Loaded) {
    if loaded.steps.as_ref().is_none_or(|(range, _)| *range != loaded.range) {
        let (lo, hi) = loaded.range;
        let (setpoint, gyro) = (loaded.axis_cols("setpoint"), loaded.axis_cols("gyroADC"));
        let steps = [0, 1, 2].map(|a| dsp::step_response(&setpoint[a]?[lo..hi], &gyro[a]?[lo..hi], loaded.sample_rate));
        loaded.steps = Some((loaded.range, steps));
    }
}

/// Высота графиков в строке оси, над которой стоит строка текста.
fn captioned_row_height(ui: &egui::Ui) -> f32 {
    row_height(ui, 3) - ui.text_style_height(&egui::TextStyle::Body) - ui.spacing().item_spacing.y
}

/// Отклик на ступеньку и частотная характеристика по осям, по видимому на кривых участку.
pub fn show_step(ui: &mut egui::Ui, loaded: &mut Loaded, compare: Option<&mut Loaded>) {
    ensure_steps(loaded);
    let steps = &loaded.steps.as_ref().unwrap().1;
    let compared = compare.map(|c| {
        ensure_steps(c);
        &c.steps.as_ref().unwrap().1
    });
    let height = captioned_row_height(ui);
    for (a, axis) in AXES.iter().enumerate() {
        let Some(step) = &steps[a] else {
            ui.colored_label(theme::MUTED, format!("{axis}: отклик не получен. Нужно не меньше 15 с движений стиков быстрее 20 °/с на выбранном участке и поля setpoint и gyroADC в логе."));
            ui.add_space(height);
            continue;
        };
        let other = compared.and_then(|c| c[a].as_ref());
        let peak = step.points.iter().map(|p| p[1]).fold(0.0, f64::max);
        let half = step.points.iter().find(|p| p[1] >= 0.5).map_or(f64::NAN, |p| p[0]);
        let band = step.bandwidth_hz().map_or("выше 100 Гц или не определена".into(), |hz| format!("{hz:.0} Гц"));
        ui.horizontal(|ui| {
            ui.label(*axis);
            ui.colored_label(theme::MUTED, format!("окон {}, пик {peak:.2}, половина задания через {half:.0} мс, полоса по уровню −3 дБ: {band}", step.windows));
        });
        ui.columns(3, |columns| {
            let x_label = |plot: egui_plot::Plot<'static>, label: &'static str| if a == 2 { plot.x_axis_label(label) } else { plot };
            x_label(new_plot(("step", a)).height(height).link_axis("step", [true, true]).include_y(0.0).include_y(1.2).y_axis_label("отклик"), "время после ступеньки, мс").show(&mut columns[0], |plot| {
                plot.hline(HLine::new("задание", 1.0).color(theme::MUTED).style(LineStyle::dashed_dense()));
                plot.line(line("gyroADC", step.points.clone()));
                if let Some(other) = other {
                    plot.line(line("gyroADC", other.points.clone()).name("сравнение").style(LineStyle::dashed_loose()));
                }
            });
            // Усиление и фаза — на отдельных графиках: у них разные единицы.
            for (column, (id, label, value)) in [("gain", "усиление, дБ", 1), ("phase", "фаза, °", 2)].into_iter().enumerate() {
                let curve = |s: &dsp::StepResponse| s.bode.iter().map(|p| [p[0], p[value]]).collect::<Vec<_>>();
                x_label(new_plot((id, a)).height(height).link_axis(id, [true, true]).y_axis_label(label), "частота, Гц").show(&mut columns[column + 1], |plot| {
                    plot.line(line("gyroADC", curve(step)));
                    if let Some(other) = other {
                        plot.line(line("gyroADC", curve(other)).name("сравнение").style(LineStyle::dashed_loose()));
                    }
                });
            }
        });
    }
}

pub struct Motors {
    /// По моторам: `(имя поля, среднее %, на максимуме % времени, на минимуме % времени)`.
    stats: Vec<(String, f32, f32, f32)>,
    spectra: Vec<Vec<[f64; 2]>>,
    /// Газ: `(средний %, 95 % времени не выше, %)`.
    throttle: Option<(f32, f32)>,
}

impl Motors {
    fn new(loaded: &Loaded) -> Self {
        let (lo, hi) = loaded.range;
        let cols: Vec<(&String, &[f32])> = loaded.log.names.iter().zip(&loaded.log.cols).filter(|(n, _)| n.starts_with("motor[")).map(|(n, c)| (n, &c[lo..hi])).collect();
        // Диапазон выхода — из заголовка лога, иначе по самим данным.
        let header: Vec<f32> = loaded.log.header("motorOutput").map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect()).unwrap_or_default();
        let (low, high) = match header[..] {
            [low, high] => (low, high),
            _ => cols.iter().flat_map(|(_, c)| c.iter()).fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v))),
        };
        let pct = |v: f32| (v - low) / (high - low) * 100.0;
        let stats = cols
            .iter()
            .map(|(name, c)| {
                let share = |hit: &dyn Fn(f32) -> bool| c.iter().filter(|&&v| hit(pct(v))).count() as f32 / c.len() as f32 * 100.0;
                ((*name).clone(), pct(c.iter().sum::<f32>() / c.len() as f32), share(&|p| p >= 99.0), share(&|p| p <= 1.0))
            })
            .collect();
        let throttle = loaded.throttle_pct().map(|t| t[lo..hi].to_vec()).filter(|t| !t.is_empty()).map(|mut t| {
            let mean = t.iter().sum::<f32>() / t.len() as f32;
            let k = t.len() * 95 / 100;
            (mean, *t.select_nth_unstable_by(k, f32::total_cmp).1)
        });
        Self { stats, spectra: cols.iter().map(|(_, c)| dsp::welch_db(c, loaded.sample_rate)).collect(), throttle }
    }
}

fn ensure_motors(loaded: &mut Loaded) {
    if loaded.motors.as_ref().is_none_or(|(range, _)| *range != loaded.range) {
        loaded.motors = Some((loaded.range, Motors::new(loaded)));
    }
}

/// Загрузка моторов и спектр их выходов, по видимому на кривых участку.
pub fn show_motors(ui: &mut egui::Ui, loaded: &mut Loaded) {
    ensure_motors(loaded);
    let motors = &loaded.motors.as_ref().unwrap().1;
    if motors.stats.is_empty() {
        ui.colored_label(theme::MUTED, "В логе нет полей motor[…]");
        return;
    }
    egui::Grid::new("motor_stats").num_columns(5).spacing([24.0, 6.0]).show(ui, |ui| {
        for title in ["", "Мотор", "Средний выход", "На максимуме", "На минимуме"] {
            ui.colored_label(theme::MUTED, title);
        }
        ui.end_row();
        for (name, mean, at_max, at_min) in &motors.stats {
            swatch(ui, name);
            ui.label(name);
            ui.label(format!("{mean:.1} %"));
            ui.label(format!("{at_max:.2} % времени"));
            ui.label(format!("{at_min:.2} % времени"));
            ui.end_row();
        }
    });
    if let Some((mean, p95)) = motors.throttle {
        ui.horizontal(|ui| {
            ui.label("Газ");
            ui.colored_label(theme::MUTED, format!("в среднем {mean:.0} %, 95 % времени не выше {p95:.0} %"));
        });
    }
    ui.add_space(4.0);
    new_plot("motor_spectrum").x_axis_label("частота, Гц").y_axis_label("выход моторов, дБ").show(ui, |plot| {
        for ((name, ..), spectrum) in motors.stats.iter().zip(&motors.spectra) {
            plot.line(line(name, spectrum.clone()));
        }
    });
}

/// Ошибка слежения одной оси: задание минус гироскоп, °/с.
pub struct TrackingError {
    rms: f32,
    /// 95 % времени ошибка по модулю не больше этого значения.
    p95: f32,
    /// Гистограмма: `[ошибка; доля времени, %]`.
    histogram: Vec<[f64; 2]>,
    /// Средняя по модулю ошибка в зависимости от заданной скорости: `[задание, °/с; ошибка, °/с]`.
    by_rate: Vec<[f64; 2]>,
}

impl TrackingError {
    fn new(setpoint: &[f32], gyro: &[f32]) -> Option<Self> {
        const BINS: usize = 81;
        const RATE_BINS: usize = 12;
        if setpoint.is_empty() {
            return None;
        }
        let error: Vec<f32> = setpoint.iter().zip(gyro).map(|(s, g)| s - g).collect();
        let p95_of = |values: &mut [f32]| *values.select_nth_unstable_by(values.len() * 95 / 100, f32::total_cmp).1;
        let p95 = p95_of(&mut error.iter().map(|e| e.abs()).collect::<Vec<_>>());
        let rms = (error.iter().map(|e| (e * e) as f64).sum::<f64>() / error.len() as f64).sqrt() as f32;
        // Гистограмма покрывает около ±2·p95: хвосты от ударов и срывов не сжимают её в полоску.
        // Значения в логе целые, поэтому ширина столбца — целое число °/с, а границы
        // столбцов лежат между целыми: иначе в соседние столбцы попадало бы разное число значений.
        let width = (4.0 * p95 / BINS as f32).ceil().max(1.0);
        let half = width * BINS as f32 / 2.0;
        let mut counts = [0u32; BINS];
        for e in &error {
            let bin = ((e + half) / width).floor() as isize;
            if (0..BINS as isize).contains(&bin) {
                counts[bin as usize] += 1;
            }
        }
        let histogram = (0..BINS)
            .flat_map(|i| {
                let (left, share) = (-half + i as f32 * width, counts[i] as f64 / error.len() as f64 * 100.0);
                [[left as f64, share], [(left + width) as f64, share]]
            })
            .collect();

        // Ошибка от скорости: до скорости, выше которой задание бывает лишь 5 % времени.
        let top_rate = p95_of(&mut setpoint.iter().map(|s| s.abs()).collect::<Vec<_>>()).max(1.0);
        let mut by_bin = [(0f64, 0u32); RATE_BINS];
        for (s, e) in setpoint.iter().zip(&error) {
            let bin = (s.abs() / top_rate * RATE_BINS as f32) as usize;
            if let Some((sum, count)) = by_bin.get_mut(bin) {
                *sum += e.abs() as f64;
                *count += 1;
            }
        }
        let by_rate = by_bin.iter().enumerate().filter(|(_, (_, count))| *count >= 50).map(|(i, (sum, count))| [(i as f64 + 0.5) * top_rate as f64 / RATE_BINS as f64, sum / *count as f64]).collect();
        Some(Self { rms, p95, histogram, by_rate })
    }
}

/// Ошибка слежения по осям, по видимому на кривых участку: распределение
/// и зависимость от заданной скорости.
pub fn show_error(ui: &mut egui::Ui, loaded: &mut Loaded) {
    if loaded.errors.as_ref().is_none_or(|(range, _)| *range != loaded.range) {
        let (lo, hi) = loaded.range;
        let (setpoint, gyro) = (loaded.axis_cols("setpoint"), loaded.axis_cols("gyroADC"));
        loaded.errors = Some((loaded.range, [0, 1, 2].map(|a| TrackingError::new(&setpoint[a]?[lo..hi], &gyro[a]?[lo..hi]))));
    }
    let errors = &loaded.errors.as_ref().unwrap().1;
    let height = captioned_row_height(ui);
    for (a, axis) in AXES.iter().enumerate() {
        let Some(error) = &errors[a] else {
            ui.colored_label(theme::MUTED, format!("{axis}: в логе нет полей setpoint и gyroADC"));
            ui.add_space(height);
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(*axis);
            ui.colored_label(theme::MUTED, format!("среднеквадратичная ошибка {:.1} °/с, 95 % времени не больше {:.1} °/с", error.rms, error.p95));
        });
        ui.columns(2, |columns| {
            let x_label = |plot: egui_plot::Plot<'static>, label: &'static str| if a == 2 { plot.x_axis_label(label) } else { plot };
            x_label(new_plot(("error", a)).height(height).link_axis("error", [true, false]).include_y(0.0).y_axis_label("доля времени, %"), "задание минус гироскоп, °/с")
                .show(&mut columns[0], |plot| plot.line(line("gyroADC", error.histogram.clone()).fill(0.0)));
            x_label(new_plot(("error_rate", a)).height(height).link_axis("error_rate", [true, false]).include_y(0.0).y_axis_label("средняя ошибка, °/с"), "заданная скорость по модулю, °/с")
                .show(&mut columns[1], |plot| plot.line(line("gyroADC", error.by_rate.clone())));
        });
    }
}

/// Узкие пики в спектре выше 40 Гц: `(частота, превышение над окружением в дБ)`, от сильных к слабым.
fn spectrum_peaks(spectrum: &[[f64; 2]]) -> Vec<(f64, f64)> {
    let Some(step) = spectrum.get(1).map(|p| p[0] - spectrum[0][0]) else { return Vec::new() };
    let around = (30.0 / step) as usize; // окружение — ±30 Гц
    let mut peaks = Vec::new();
    for i in 1..spectrum.len().saturating_sub(1) {
        let [hz, db] = spectrum[i];
        if hz < 40.0 || db < spectrum[i - 1][1] || db < spectrum[i + 1][1] {
            continue;
        }
        let mut near: Vec<f64> = spectrum[i.saturating_sub(around)..(i + around).min(spectrum.len())].iter().map(|p| p[1]).collect();
        let mid = near.len() / 2;
        let baseline = *near.select_nth_unstable_by(mid, f64::total_cmp).1;
        if db - baseline > 8.0 {
            peaks.push((hz, db - baseline));
        }
    }
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    peaks
}

/// Сводка по видимому на кривых участку.
pub struct Overview {
    /// Автоматические находки: `(важно ли, текст)`.
    pub findings: Vec<(bool, String)>,
    /// СКО составляющих регулятора по осям: `(поле, [Roll, Pitch, Yaw])`.
    balance: Vec<(&'static str, [Option<f32>; 3])>,
}

fn findings(loaded: &mut Loaded) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let nyquist = loaded.sample_rate / 2.0;
    if nyquist < 500.0 {
        out.push((true, format!("Лог записан с частотой {:.0} Гц: шум выше {nyquist:.0} Гц в нём не виден, а более высокие частоты отражаются в видимую полосу. Для анализа шума записывайте лог на 2 кГц и выше.", loaded.sample_rate)));
    }
    if loaded.log.corrupt >= 5 {
        out.push((loaded.log.corrupt >= 50, format!("В записи {} повреждённых участков: носитель не успевает или в файле остались куски старых данных. Вокруг разрывов графикам доверять нельзя.", loaded.log.corrupt)));
    }

    ensure_motors(loaded);
    let stats = &loaded.motors.as_ref().unwrap().1.stats;
    for (name, _, at_max, _) in stats {
        if *at_max > 1.0 {
            out.push((true, format!("{name} упирается в максимум {at_max:.1} % времени: запаса тяги не хватает, в эти моменты управление по осям теряется.")));
        }
    }
    if stats.len() >= 3 {
        let mean_all = stats.iter().map(|s| s.1).sum::<f32>() / stats.len() as f32;
        for (name, mean, ..) in stats {
            if (mean - mean_all).abs() > 8.0 {
                let how = if *mean > mean_all { "нагружен сильнее" } else { "нагружен слабее" };
                out.push((false, format!("{name} {how} остальных: средний выход {mean:.0} % при среднем по моторам {mean_all:.0} %. Проверьте центровку, пропеллер и мотор.")));
            }
        }
    }

    for (a, ms) in loaded.delays().into_iter().enumerate() {
        if let Some(ms) = ms.filter(|ms| *ms > 4.0) {
            out.push((false, format!("{}: фильтры гироскопа задерживают сигнал на {ms:.1} мс. Чем больше задержка, тем ниже достижимая резкость настройки.", AXES[a])));
        }
    }

    for (base, by_axis) in loaded.spectra().clone() {
        if base != "gyroADC" {
            continue;
        }
        for (a, spectrum) in by_axis.iter().enumerate() {
            for (hz, above) in spectrum_peaks(spectrum).into_iter().take(2) {
                out.push((above > 15.0, format!("{}: узкий пик на {hz:.0} Гц, на {above:.0} дБ выше окружающего шума, остаётся после фильтров гироскопа.", AXES[a])));
            }
        }
    }

    ensure_steps(loaded);
    for (a, step) in loaded.steps.as_ref().unwrap().1.iter().enumerate() {
        let Some(step) = step else { continue };
        let peak = step.points.iter().map(|p| p[1]).fold(0.0, f64::max);
        let half = step.points.iter().find(|p| p[1] >= 0.5).map_or(f64::NAN, |p| p[0]);
        if peak > 3.0 {
            // Втрое выше задания контур не отвечает: значит, гироскоп на этом участке
            // с заданием почти не связан (ручной режим, самолёт, срыв) и оценка не годится.
            out.push((false, format!("{}: отклик на ступеньку получился неправдоподобным (пик {peak:.1}) — на этом участке гироскоп плохо следует заданию, оценке отклика доверять нельзя.", AXES[a])));
            continue;
        }
        if peak > 1.2 {
            out.push((true, format!("{}: перерегулирование {:.0} % в отклике на ступеньку.", AXES[a], (peak - 1.0) * 100.0)));
        }
        if half > 40.0 {
            out.push((false, format!("{}: медленный отклик — половина задания достигается через {half:.0} мс.", AXES[a])));
        }
    }
    out
}

pub fn overview(loaded: &mut Loaded) -> &Overview {
    if loaded.overview.as_ref().is_none_or(|(range, _)| *range != loaded.range) {
        let (lo, hi) = loaded.range;
        let rms = |c: &[f32]| (c[lo..hi].iter().map(|v| (v * v) as f64).sum::<f64>() / (hi - lo).max(1) as f64).sqrt() as f32;
        let balance = ["axisP", "axisI", "axisD", "axisF"].into_iter().filter(|base| loaded.axis_fields.iter().any(|f| f.base == *base)).map(|base| (base, loaded.axis_cols(base).map(|c| c.map(rms)))).collect();
        loaded.overview = Some((loaded.range, Overview { findings: findings(loaded), balance }));
    }
    &loaded.overview.as_ref().unwrap().1
}

pub fn show_overview(ui: &mut egui::Ui, loaded: &mut Loaded) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let overview = overview(loaded);
        ui.colored_label(theme::MUTED, "Всё считается по участку, выбранному масштабом на вкладке «Кривые». Находки — подсказки, куда смотреть, а не диагноз.");
        ui.add_space(6.0);
        ui.heading("Находки");
        if overview.findings.is_empty() {
            ui.label("По проверяемым признакам замечаний нет.");
        }
        for (important, text) in &overview.findings {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(if *important { theme::WARN } else { theme::MUTED }, if *important { "Важно" } else { "Заметка" });
                ui.label(text);
            });
        }

        if !overview.balance.is_empty() {
            ui.add_space(12.0);
            ui.heading("Вклад составляющих регулятора");
            ui.colored_label(theme::MUTED, "Среднеквадратичное значение каждой составляющей выхода регулятора.");
            egui::Grid::new("balance").num_columns(5).spacing([24.0, 6.0]).show(ui, |ui| {
                for title in ["", "Составляющая", AXES[0], AXES[1], AXES[2]] {
                    ui.colored_label(theme::MUTED, title);
                }
                ui.end_row();
                for (base, by_axis) in &overview.balance {
                    swatch(ui, base);
                    ui.label(*base);
                    for value in by_axis {
                        ui.label(value.map_or("нет".into(), |v| format!("{v:.1}")));
                    }
                    ui.end_row();
                }
            });
        }

        if !loaded.log.events.is_empty() {
            ui.add_space(12.0);
            ui.heading("События в логе");
            egui::Grid::new("events").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                for (time, event) in &loaded.log.events {
                    ui.colored_label(theme::MUTED, format!("{time:.2} с"));
                    ui.label(event);
                    ui.end_row();
                }
            });
        }
    });
}

/// Параметры из заголовка лога; при сравнении — рядом значения второго лога.
pub fn show_settings(ui: &mut egui::Ui, loaded: &Loaded, compare: Option<&Loaded>, view: &mut View) {
    ui.horizontal(|ui| {
        ui.label("Параметр");
        ui.add(egui::TextEdit::singleline(&mut view.settings_filter).hint_text("часть названия, например lpf").desired_width(260.0));
        if compare.is_some() {
            ui.checkbox(&mut view.only_different, "Только отличающиеся");
        }
    });
    let needle = view.settings_filter.to_lowercase();
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("settings").num_columns(3).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
            ui.colored_label(theme::MUTED, "Параметр");
            ui.colored_label(theme::MUTED, "Этот лог");
            if compare.is_some() {
                ui.colored_label(theme::MUTED, "Лог сравнения");
            }
            ui.end_row();
            // Описания полей кадров — служебные, к настройкам дрона не относятся.
            for (name, value) in loaded.log.headers.iter().filter(|(n, _)| !n.starts_with("Field ") && n.to_lowercase().contains(&needle)) {
                let other = compare.map(|c| c.log.header(name));
                let differs = other.is_some_and(|o| o != Some(value.as_str()));
                if view.only_different && compare.is_some() && !differs {
                    continue;
                }
                ui.label(name);
                ui.label(value);
                if let Some(other) = other {
                    ui.colored_label(if differs { theme::WARN } else { theme::MUTED }, other.unwrap_or("нет в логе"));
                }
                ui.end_row();
            }
        });
    });
}

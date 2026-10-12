//! Вкладки «Обзор», «Отклик», «Ошибка», «Моторы» и «Настройки».

use eframe::egui;
use egui_plot::{HLine, LineStyle};

use crate::{AXES, Loaded, StepSplit, View, dsp, line, new_plot, row_height, say, swatch, theme};

pub type Steps = [Option<dsp::StepResponse>; 3];

/// Печатает результаты анализа первой записи таблицей — для сверки с другими
/// программами: `step,ось,мс,отклик`; `bode,ось,Гц,дБ,градусы`;
/// `margin,ось,частота среза Гц,запас по фазе °,пик ошибки дБ,его частота Гц`;
/// `delay,ось,мс` (задержка фильтров гироскопа); `psd,сигнал,ось,Гц,дБ`.
pub fn print_response(loaded: &mut Loaded) {
    for (axis, delay) in AXES.iter().zip(loaded.delays()) {
        if let Some(ms) = delay {
            say(format_args!("delay,{axis},{ms:.3}"));
        }
    }
    for (base, by_axis) in loaded.spectra().clone() {
        for (axis, spectrum) in AXES.iter().zip(&by_axis) {
            for p in spectrum {
                say(format_args!("psd,{base},{axis},{:.3},{:.3}", p[0], p[1]));
            }
        }
    }
    ensure_steps(loaded);
    for (axis, step) in AXES.iter().zip(&loaded.steps.as_ref().unwrap().1) {
        for p in step.iter().flat_map(|s| &s.points) {
            say(format_args!("step,{axis},{:.2},{:.4}", p[0], p[1]));
        }
        for p in step.iter().flat_map(|s| &s.bode) {
            say(format_args!("bode,{axis},{:.3},{:.3},{:.2}", p[0], p[1], p[2]));
        }
        if let Some(m) = step.as_ref().and_then(dsp::StepResponse::margins) {
            say(format_args!("margin,{axis},{:.2},{:.1},{:.2},{:.2}", m.crossover_hz, m.phase_margin_deg, m.error_peak_db, m.error_peak_hz));
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

/// Отклики по группам окон: `(название группы, отклики по осям)`.
pub type SplitSteps = Vec<(String, Steps)>;

/// Считает отклик отдельно по группам окон: по скорости движения стиков
/// (плавные и резкие) или по газу (три равные по времени трети).
fn ensure_split_steps(loaded: &mut Loaded, split: StepSplit) {
    if loaded.split_steps.as_ref().is_some_and(|(range, (s, _))| *range == loaded.range && *s == split) {
        return;
    }
    const FAST: f32 = 500.0; // °/с, как в PID-Analyzer
    let ((lo, hi), fs) = (loaded.range, loaded.sample_rate);
    let (setpoint, gyro) = (loaded.axis_cols("setpoint"), loaded.axis_cols("gyroADC"));
    let per_axis = |keep: &dyn Fn(usize, &[f32]) -> bool| [0, 1, 2].map(|a| dsp::step_response_where(&setpoint[a]?[lo..hi], &gyro[a]?[lo..hi], fs, keep));
    let top = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let groups: SplitSteps = match (split, loaded.throttle_pct()) {
        (StepSplit::Rate, _) => vec![
            (tf!("плавные движения, до {FAST:.0} °/с", "smooth moves, below {FAST:.0} °/s"), per_axis(&|_, x| top(x) < FAST)),
            (tf!("резкие, от {FAST:.0} °/с", "sharp moves, from {FAST:.0} °/s"), per_axis(&|_, x| top(x) >= FAST)),
        ],
        (StepSplit::Throttle, Some(throttle)) => {
            let throttle = &throttle[lo..hi];
            let mut sorted = throttle.to_vec();
            sorted.sort_unstable_by(f32::total_cmp);
            let (low, high) = (sorted[sorted.len() / 3], sorted[sorted.len() * 2 / 3]);
            // Газ окна — средний за его 2 с.
            let window = (fs * 2.0) as usize;
            let mean = |start: usize| throttle[start..(start + window).min(throttle.len())].iter().sum::<f32>() / window as f32;
            vec![
                (tf!("малый газ, до {low:.0} %", "low throttle, below {low:.0} %"), per_axis(&|start, _| mean(start) < low)),
                (tf!("средний газ, {low:.0}–{high:.0} %", "mid throttle, {low:.0}–{high:.0} %"), per_axis(&|start, _| (low..high).contains(&mean(start)))),
                (tf!("большой газ, от {high:.0} %", "high throttle, from {high:.0} %"), per_axis(&|start, _| mean(start) >= high)),
            ]
        }
        _ => Vec::new(),
    };
    loaded.split_steps = Some((loaded.range, (split, groups)));
}

/// Отклик на ступеньку и частотная характеристика по осям, по видимому на кривых участку.
pub fn show_step(ui: &mut egui::Ui, loaded: &mut Loaded, compare: Option<&mut Loaded>, view: &mut View) {
    ui.horizontal(|ui| {
        ui.label(t!("Отклик", "Response"));
        ui.selectable_value(&mut view.step_split, StepSplit::None, t!("Общий", "Overall"));
        ui.selectable_value(&mut view.step_split, StepSplit::Rate, t!("По скорости стиков", "By stick rate")).on_hover_text(t!("Отдельно для плавных и резких движений. Расходятся — настройка ведёт себя по-разному на малых и больших скоростях.", "Separately for smooth and sharp moves. If they differ, the tune behaves differently at low and high rates."));
        ui.selectable_value(&mut view.step_split, StepSplit::Throttle, t!("По газу", "By throttle")).on_hover_text(t!("Отдельно для малого, среднего и большого газа. Расходятся — усиление контура плывёт с газом (смотрите TPA).", "Separately for low, mid and high throttle. If they differ, loop gain drifts with throttle (see TPA)."));
    });
    ensure_steps(loaded);
    ensure_split_steps(loaded, view.step_split);
    let steps = &loaded.steps.as_ref().unwrap().1;
    let groups = &loaded.split_steps.as_ref().unwrap().1.1;
    let compared = compare.map(|c| {
        ensure_steps(c);
        &c.steps.as_ref().unwrap().1
    });
    let height = captioned_row_height(ui);
    for (a, axis) in AXES.iter().enumerate() {
        // Кривые оси: общий отклик либо отклики по группам, каждая своим цветом.
        let curves: Vec<(egui::Color32, &str, &dsp::StepResponse)> = if groups.is_empty() {
            steps[a].iter().map(|s| (theme::series_color("gyroADC"), "gyroADC", s)).collect()
        } else {
            groups.iter().enumerate().filter_map(|(i, (name, by_axis))| Some((theme::nth_series(i), name.as_str(), by_axis[a].as_ref()?))).collect()
        };
        if curves.is_empty() {
            ui.colored_label(theme::MUTED, tf!("{axis}: отклик не получен. Нужно не меньше 15 с движений стиков быстрее 20 °/с на выбранном участке и поля setpoint и gyroADC в логе.", "{axis}: no response estimate. It needs at least 15 s of stick moves faster than 20 °/s in the selected range, and the setpoint and gyroADC fields in the log."));
            ui.add_space(height);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(*axis);
            if groups.is_empty() {
                let step = curves[0].2;
                let peak = step.points.iter().map(|p| p[1]).fold(0.0, f64::max);
                let half = step.points.iter().find(|p| p[1] >= 0.5).map_or(f64::NAN, |p| p[0]);
                let band = step.bandwidth_hz().map_or(t!("выше 100 Гц или не определена", "above 100 Hz or undefined").into(), |hz| tf!("{hz:.0} Гц", "{hz:.0} Hz"));
                let margin = step.margins().map_or(t!("не определён", "undefined").into(), |m| tf!("{:.0}° на {:.0} Гц, пик ошибки {:+.0} дБ на {:.0} Гц", "{:.0}° at {:.0} Hz, error peak {:+.0} dB at {:.0} Hz", m.phase_margin_deg, m.crossover_hz, m.error_peak_db, m.error_peak_hz));
                ui.colored_label(theme::MUTED, tf!("окон {}, пик {peak:.2}, половина задания через {half:.0} мс, полоса по −3 дБ: {band}, запас по фазе: {margin}", "{} windows, peak {peak:.2}, half of setpoint in {half:.0} ms, −3 dB bandwidth: {band}, phase margin: {margin}", step.windows)).on_hover_text(
                    t!("Запас по фазе — насколько контур далёк от раскачки: меньше 45° — близко. Пик ошибки — во сколько раз контур усиливает рассогласование на худшей частоте. Оба считаются по измеренной связи задания и гироскопа в предположении, что feedforward мал.", "Phase margin shows how far the loop is from oscillating: below 45° is close. Error peak is how much the loop amplifies the mismatch at the worst frequency. Both are derived from the measured setpoint-to-gyro relation, assuming feedforward is small."),
                );
            } else {
                for (i, (name, by_axis)) in groups.iter().enumerate() {
                    crate::swatch_color(ui, theme::nth_series(i));
                    match &by_axis[a] {
                        Some(step) => ui.colored_label(theme::MUTED, tf!("{name}: пик {:.2}, окон {}", "{name}: peak {:.2}, {} windows", step.points.iter().map(|p| p[1]).fold(0.0, f64::max), step.windows)),
                        None => ui.colored_label(theme::MUTED, tf!("{name}: мало данных", "{name}: not enough data")),
                    };
                }
            }
        });
        let other = compared.filter(|_| groups.is_empty()).and_then(|c| c[a].as_ref());
        let crossover = curves[0].2.margins().filter(|_| groups.is_empty()).map(|m| m.crossover_hz);
        ui.columns(3, |columns| {
            let x_label = |plot: egui_plot::Plot<'static>, label: &'static str| if a == 2 { plot.x_axis_label(label) } else { plot };
            x_label(new_plot(("step", a)).height(height).link_axis("step", [true, true]).include_y(0.0).include_y(1.2).y_axis_label(t!("отклик", "response")), t!("время после ступеньки, мс", "time after step, ms")).show(&mut columns[0], |plot| {
                plot.hline(HLine::new(t!("задание", "setpoint"), 1.0).color(theme::MUTED).style(LineStyle::dashed_dense()));
                for (color, name, step) in &curves {
                    plot.line(line(name, step.points.clone()).color(*color));
                }
                if let Some(other) = other {
                    plot.line(line("gyroADC", other.points.clone()).name(t!("сравнение", "comparison")).style(LineStyle::dashed_loose()));
                }
            });
            // Усиление и фаза — на отдельных графиках: у них разные единицы.
            for (column, (id, label, value)) in [("gain", t!("усиление, дБ", "gain, dB"), 1), ("phase", t!("фаза, °", "phase, °"), 2)].into_iter().enumerate() {
                let curve = |s: &dsp::StepResponse| s.bode.iter().map(|p| [p[0], p[value]]).collect::<Vec<_>>();
                x_label(new_plot((id, a)).height(height).link_axis(id, [true, true]).y_axis_label(label), t!("частота, Гц", "frequency, Hz")).show(&mut columns[column + 1], |plot| {
                    for (color, name, step) in &curves {
                        plot.line(line(name, curve(step)).color(*color));
                    }
                    if let Some(other) = other {
                        plot.line(line("gyroADC", curve(other)).name(t!("сравнение", "comparison")).style(LineStyle::dashed_loose()));
                    }
                    if let Some(hz) = crossover {
                        plot.vline(egui_plot::VLine::new(t!("частота среза контура", "loop crossover frequency"), hz).color(theme::MUTED).style(LineStyle::dashed_dense()).width(1.0));
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
    /// По моторам (в том же порядке, что `stats`): средние обороты, Гц, и вибрация
    /// гироскопа на частоте вращения по осям. `None` — в логе нет оборотов этого мотора.
    rotation: Vec<Option<(f32, [Option<dsp::RotationVibration>; 3])>>,
    /// Вибрация считалась по сырому гироскопу; иначе — по отфильтрованному, и она занижена.
    rotation_from_raw: bool,
}

impl Motors {
    /// Вибрация мотора в плоскости рамы: Roll и Pitch вместе (на Yaw дисбаланс почти не действует).
    /// Вторым числом — фон, с которым её надо сравнивать.
    fn shake(&self, motor: usize) -> Option<(f64, f64)> {
        let (_, by_axis) = self.rotation.get(motor)?.as_ref()?;
        let (roll, pitch) = (by_axis[0].as_ref()?, by_axis[1].as_ref()?);
        Some((roll.amplitude.hypot(pitch.amplitude), roll.floor.hypot(pitch.floor)))
    }

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
        let rotation_from_raw = loaded.axis_fields.iter().any(|f| f.base == "gyroUnfilt");
        let gyro = loaded.axis_cols(if rotation_from_raw { "gyroUnfilt" } else { "gyroADC" });
        let rotation = cols
            .iter()
            .map(|(name, _)| {
                // motor[3] ↔ eRPM[3]
                let hz: Vec<f32> = loaded.log.col(&name.replace("motor", "eRPM"))?[lo..hi].iter().map(|v| v * loaded.erpm_to_hz()).collect();
                let mean = hz.iter().sum::<f32>() / hz.len().max(1) as f32;
                Some((mean, [0, 1, 2].map(|a| dsp::rotation_vibration(&gyro[a]?[lo..hi], &hz, loaded.sample_rate))))
            })
            .collect();
        Self { stats, spectra: cols.iter().map(|(_, c)| dsp::welch_db(c, loaded.sample_rate)).collect(), throttle, rotation, rotation_from_raw }
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
        ui.colored_label(theme::MUTED, t!("В логе нет полей motor[…]", "The log has no motor[…] fields"));
        return;
    }
    let has_rotation = motors.rotation.iter().any(Option::is_some);
    egui::Grid::new("motor_stats").num_columns(9).spacing([24.0, 6.0]).show(ui, |ui| {
        for title in ["", t!("Мотор", "Motor"), t!("Средний выход", "Mean output"), t!("На максимуме", "At maximum"), t!("На минимуме", "At minimum")] {
            ui.colored_label(theme::MUTED, title);
        }
        if has_rotation {
            for title in [t!("Обороты", "Speed"), t!("Вибрация Roll", "Roll vibration"), t!("Вибрация Pitch", "Pitch vibration"), t!("Вибрация Yaw", "Yaw vibration")] {
                ui.colored_label(theme::MUTED, title);
            }
        }
        ui.end_row();
        for ((name, mean, at_max, at_min), rotation) in motors.stats.iter().zip(&motors.rotation) {
            swatch(ui, name);
            ui.label(name);
            ui.label(format!("{mean:.1} %"));
            ui.label(tf!("{at_max:.2} % времени", "{at_max:.2} % of time"));
            ui.label(tf!("{at_min:.2} % времени", "{at_min:.2} % of time"));
            if let Some((hz, by_axis)) = rotation {
                ui.label(tf!("{hz:.0} Гц", "{hz:.0} Hz"));
                for vibration in by_axis {
                    match vibration {
                        // Вибрация, не превышающая фон вдвое, показана приглушённо: мотор в ней не виден.
                        Some(v) if v.amplitude > 2.0 * v.floor => ui.label(tf!("{:.1} °/с", "{:.1} °/s", v.amplitude)),
                        Some(v) => ui.colored_label(theme::MUTED, tf!("{:.1} °/с", "{:.1} °/s", v.amplitude)),
                        None => ui.colored_label(theme::MUTED, t!("нет данных", "no data")),
                    };
                }
            }
            ui.end_row();
        }
    });
    if has_rotation {
        let source = if motors.rotation_from_raw { t!("по сырому гироскопу", "from the raw gyro") } else { t!("по отфильтрованному гироскопу — RPM-фильтр эту частоту давит, значения занижены", "from the filtered gyro — the RPM filter suppresses this frequency, so the values are too low") };
        ui.colored_label(theme::MUTED, tf!("Вибрация — амплитуда колебаний гироскопа на частоте вращения мотора, {source}. Приглушённые значения не отличаются от фона.", "Vibration is the gyro oscillation amplitude at the motor's rotation frequency, {source}. Dimmed values do not stand out from the background."));
    }
    if let Some((mean, p95)) = motors.throttle {
        ui.horizontal(|ui| {
            ui.label(t!("Газ", "Throttle"));
            ui.colored_label(theme::MUTED, tf!("в среднем {mean:.0} %, 95 % времени не выше {p95:.0} %", "{mean:.0} % on average, at or below {p95:.0} % for 95 % of the time"));
        });
    }
    ui.add_space(4.0);
    new_plot("motor_spectrum").x_axis_label(t!("частота, Гц", "frequency, Hz")).y_axis_label(t!("выход моторов, дБ", "motor output, dB")).show(ui, |plot| {
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
            ui.colored_label(theme::MUTED, tf!("{axis}: в логе нет полей setpoint и gyroADC", "{axis}: the log has no setpoint and gyroADC fields"));
            ui.add_space(height);
            continue;
        };
        ui.horizontal(|ui| {
            ui.label(*axis);
            ui.colored_label(theme::MUTED, tf!("среднеквадратичная ошибка {:.1} °/с, 95 % времени не больше {:.1} °/с", "RMS error {:.1} °/s, at or below {:.1} °/s for 95 % of the time", error.rms, error.p95));
        });
        ui.columns(2, |columns| {
            let x_label = |plot: egui_plot::Plot<'static>, label: &'static str| if a == 2 { plot.x_axis_label(label) } else { plot };
            x_label(new_plot(("error", a)).height(height).link_axis("error", [true, false]).include_y(0.0).y_axis_label(t!("доля времени, %", "share of time, %")), t!("задание минус гироскоп, °/с", "setpoint minus gyro, °/s"))
                .show(&mut columns[0], |plot| plot.line(line("gyroADC", error.histogram.clone()).fill(0.0)));
            x_label(new_plot(("error_rate", a)).height(height).link_axis("error_rate", [true, false]).include_y(0.0).y_axis_label(t!("средняя ошибка, °/с", "mean error, °/s")), t!("заданная скорость по модулю, °/с", "absolute commanded rate, °/s"))
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
    /// Раскачка после сбросов газа по осям.
    prop_wash: [Option<dsp::PropWash>; 3],
}

fn findings(loaded: &mut Loaded, prop_wash: &[Option<dsp::PropWash>; 3], balance: &[(&'static str, [Option<f32>; 3])]) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let nyquist = loaded.sample_rate / 2.0;
    if nyquist < 500.0 {
        out.push((true, tf!("Лог записан с частотой {:.0} Гц: шум выше {nyquist:.0} Гц в нём не виден, а более высокие частоты отражаются в видимую полосу. Для анализа шума записывайте лог на 2 кГц и выше.", "The log was recorded at {:.0} Hz: noise above {nyquist:.0} Hz is not visible in it, and higher frequencies fold back into the visible band. For noise analysis, log at 2 kHz or higher.", loaded.sample_rate)));
    }
    if loaded.log.corrupt >= 5 {
        out.push((loaded.log.corrupt >= 50, tf!("В записи {} повреждённых участков: носитель не успевает или в файле остались куски старых данных. Вокруг разрывов графикам доверять нельзя.", "The recording has {} damaged regions: the storage cannot keep up, or the file contains leftovers of old data. Do not trust the charts around the gaps.", loaded.log.corrupt)));
    }

    ensure_motors(loaded);
    let stats = &loaded.motors.as_ref().unwrap().1.stats;
    for (name, _, at_max, _) in stats {
        if *at_max > 1.0 {
            out.push((true, tf!("{name} упирается в максимум {at_max:.1} % времени: запаса тяги не хватает, в эти моменты управление по осям теряется.", "{name} hits the maximum {at_max:.1} % of the time: there is not enough thrust headroom, and axis control is lost at those moments.")));
        }
    }
    if stats.len() >= 3 {
        let mean_all = stats.iter().map(|s| s.1).sum::<f32>() / stats.len() as f32;
        for (name, mean, ..) in stats {
            if (mean - mean_all).abs() > 8.0 {
                let how = if *mean > mean_all { t!("нагружен сильнее", "more") } else { t!("нагружен слабее", "less") };
                out.push((false, tf!("{name} {how} остальных: средний выход {mean:.0} % при среднем по моторам {mean_all:.0} %. Проверьте центровку, пропеллер и мотор.", "{name} is loaded {how} than the others: mean output {mean:.0} % against a motor average of {mean_all:.0} %. Check the centre of gravity, the prop and the motor.")));
            }
        }
    }

    // Мотор, который трясёт заметно сильнее остальных.
    let motors = &loaded.motors.as_ref().unwrap().1;
    let shakes: Vec<Option<(f64, f64)>> = (0..motors.stats.len()).map(|m| motors.shake(m)).collect();
    if shakes.iter().flatten().count() >= 3 {
        for (m, (shake, floor)) in shakes.iter().enumerate().filter_map(|(m, s)| Some((m, (*s)?))) {
            let mut others: Vec<f64> = shakes.iter().enumerate().filter(|(i, _)| *i != m).filter_map(|(_, s)| Some((*s)?.0)).collect();
            let mid = others.len() / 2;
            let typical = *others.select_nth_unstable_by(mid, f64::total_cmp).1;
            // Пороги с запасом: разница вдвое встречается и у исправных моторов,
            // а вибрация слабее 3 °/с на управление не влияет.
            if shake > 2.5 * typical && shake > 2.0 * floor && shake > 3.0 {
                let caveat = if motors.rotation_from_raw { "" } else { t!(" Оценка сделана по отфильтрованному гироскопу и занижена.", " The estimate comes from the filtered gyro and is too low.") };
                out.push((shake > 4.0 * typical, tf!("{}: вибрация на частоте его вращения {shake:.1} °/с — в {:.1} раза больше, чем у остальных моторов. Проверьте пропеллер, вал и подшипники этого мотора.{caveat}", "{}: vibration at its rotation frequency is {shake:.1} °/s — {:.1} times that of the other motors. Check this motor's prop, shaft and bearings.{caveat}", motors.stats[m].0, shake / typical)));
            }
        }
    }

    for (a, ms) in loaded.delays().into_iter().enumerate() {
        if let Some(ms) = ms.filter(|ms| *ms > 4.0) {
            out.push((false, tf!("{}: фильтры гироскопа задерживают сигнал на {ms:.1} мс. Чем больше задержка, тем ниже достижимая резкость настройки.", "{}: gyro filters delay the signal by {ms:.1} ms. The longer the delay, the less sharp a tune can be.", AXES[a])));
        }
    }

    // Карты «частота × обороты» по осям — чтобы сказать, чей пик: моторов или рамы.
    let (lo, hi) = loaded.range;
    let motor_hz = loaded.motor_hz().map(|hz| hz[lo..hi].to_vec());
    let rpm_max = motor_hz.iter().flatten().copied().fold(1.0, f32::max);
    let rpm_pct: Option<Vec<f32>> = motor_hz.map(|hz| hz.iter().map(|v| v / rpm_max * 100.0).collect());
    let by_rpm = loaded.axis_cols("gyroADC").map(|c| dsp::binned_heatmap(&c?[lo..hi], rpm_pct.as_deref()?, loaded.sample_rate));
    let mut peaks_without_rpm = false;
    for (base, by_axis) in loaded.spectra().clone() {
        if base != "gyroADC" {
            continue;
        }
        for (a, spectrum) in by_axis.iter().enumerate() {
            for (hz, above) in spectrum_peaks(spectrum).into_iter().take(2) {
                let source = match by_rpm[a].as_ref().map(|grid| dsp::peak_source(grid, rpm_max as f64, nyquist, hz)) {
                    Some(dsp::PeakSource::Harmonic(k)) => tf!(" Частота следует за оборотами: это {k}-я гармоника моторов — шум моторов и пропеллеров, а не рамы.", " The frequency follows motor speed: it is motor harmonic {k} — noise from motors and props, not the frame."),
                    Some(dsp::PeakSource::Fixed) => t!(" Частота от оборотов не зависит: это резонанс рамы или крепления, а не моторы.", " The frequency does not depend on motor speed: it is a frame or mount resonance, not the motors.").into(),
                    Some(dsp::PeakSource::Unclear) => String::new(),
                    None => {
                        peaks_without_rpm = true;
                        String::new()
                    }
                };
                out.push((above > 15.0, tf!("{}: узкий пик на {hz:.0} Гц, на {above:.0} дБ выше окружающего шума, остаётся после фильтров гироскопа.{source}", "{}: a narrow peak at {hz:.0} Hz, {above:.0} dB above the surrounding noise, remains after the gyro filters.{source}", AXES[a])));
            }
        }
    }
    if peaks_without_rpm {
        out.push((false, t!("В логе нет оборотов моторов (eRPM), поэтому нельзя сказать, чьи это пики — моторов или рамы. Обороты пишутся при включённом bidirectional DShot.", "The log has no motor speed (eRPM), so it is impossible to tell whether these peaks come from the motors or the frame. Motor speed is logged when bidirectional DShot is on.").into()));
    }

    for (a, wash) in prop_wash.iter().enumerate() {
        if let Some(wash) = wash.as_ref().filter(|w| w.ratio > 2.0) {
            out.push((wash.ratio > 4.0, tf!("{}: после сбросов газа колебания в полосе 20–90 Гц в {:.1} раза сильнее обычного — prop wash (сбросов газа: {}).", "{}: after throttle chops, oscillation in the 20–90 Hz band is {:.1} times stronger than usual — prop wash (throttle chops: {}).", AXES[a], wash.ratio, wash.events)));
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
            out.push((false, tf!("{}: отклик на ступеньку получился неправдоподобным (пик {peak:.1}) — на этом участке гироскоп плохо следует заданию, оценке отклика доверять нельзя.", "{}: the step response came out implausible (peak {peak:.1}) — the gyro barely follows the setpoint in this range, so the response estimate cannot be trusted.", AXES[a])));
            continue;
        }
        if peak > 1.2 {
            out.push((true, tf!("{}: перерегулирование {:.0} % в отклике на ступеньку.", "{}: {:.0} % overshoot in the step response.", AXES[a], (peak - 1.0) * 100.0)));
        }
        if half > 40.0 {
            out.push((false, tf!("{}: медленный отклик — половина задания достигается через {half:.0} мс.", "{}: slow response — half of the setpoint is reached in {half:.0} ms.", AXES[a])));
        }
        // Запас по фазе выводится из связи «задание → гироскоп»; при заметном feedforward
        // (больше половины от P) эта связь идёт в обход обратной связи и оценка не годится.
        let term = |base: &str| balance.iter().find(|(b, _)| *b == base).and_then(|(_, by_axis)| by_axis[a]).unwrap_or(0.0);
        if let Some(margins) = step.margins().filter(|m| m.phase_margin_deg < 45.0 && term("axisF") < 0.5 * term("axisP")) {
            out.push((margins.phase_margin_deg < 30.0, tf!("{}: запас по фазе {:.0}° на {:.0} Гц — контур близок к раскачке (желательно не меньше 45°).", "{}: phase margin {:.0}° at {:.0} Hz — the loop is close to oscillating (at least 45° is desirable).", AXES[a], margins.phase_margin_deg, margins.crossover_hz)));
        }
    }
    out
}

pub fn overview(loaded: &mut Loaded) -> &Overview {
    if loaded.overview.as_ref().is_none_or(|(range, _)| *range != loaded.range) {
        let (lo, hi) = loaded.range;
        let rms = |c: &[f32]| (c[lo..hi].iter().map(|v| (v * v) as f64).sum::<f64>() / (hi - lo).max(1) as f64).sqrt() as f32;
        let balance: Vec<(&'static str, [Option<f32>; 3])> = ["axisP", "axisI", "axisD", "axisF"].into_iter().filter(|base| loaded.axis_fields.iter().any(|f| f.base == *base)).map(|base| (base, loaded.axis_cols(base).map(|c| c.map(rms)))).collect();
        let throttle = loaded.throttle_pct();
        let prop_wash = loaded.axis_cols(crate::TRACKING_ERROR).map(|error| dsp::prop_wash(&throttle.as_deref()?[lo..hi], &error?[lo..hi], loaded.sample_rate));
        loaded.overview = Some((loaded.range, Overview { findings: findings(loaded, &prop_wash, &balance), balance, prop_wash }));
    }
    &loaded.overview.as_ref().unwrap().1
}

pub fn show_overview(ui: &mut egui::Ui, loaded: &mut Loaded) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let overview = overview(loaded);
        ui.colored_label(theme::MUTED, t!("Всё считается по участку, выбранному масштабом на вкладке «Кривые». Находки — подсказки, куда смотреть, а не диагноз.", "Everything is computed over the range selected by zooming on the Curves tab. Findings are hints on where to look, not a diagnosis."));
        ui.add_space(6.0);
        ui.heading(t!("Находки", "Findings"));
        if overview.findings.is_empty() {
            ui.label(t!("По проверяемым признакам замечаний нет.", "Nothing to report on the checked criteria."));
        }
        for (important, text) in &overview.findings {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(if *important { theme::WARN } else { theme::MUTED }, if *important { t!("Важно", "Important") } else { t!("Заметка", "Note") });
                ui.label(text);
            });
        }

        if !overview.balance.is_empty() {
            ui.add_space(12.0);
            ui.heading(t!("Вклад составляющих регулятора", "Controller term contributions"));
            ui.colored_label(theme::MUTED, t!("Среднеквадратичное значение каждой составляющей выхода регулятора.", "RMS value of each term of the controller output."));
            egui::Grid::new("balance").num_columns(5).spacing([24.0, 6.0]).show(ui, |ui| {
                for title in ["", t!("Составляющая", "Term"), AXES[0], AXES[1], AXES[2]] {
                    ui.colored_label(theme::MUTED, title);
                }
                ui.end_row();
                for (base, by_axis) in &overview.balance {
                    swatch(ui, base);
                    ui.label(*base);
                    for value in by_axis {
                        ui.label(value.map_or(t!("нет", "none").into(), |v| format!("{v:.1}")));
                    }
                    ui.end_row();
                }
            });
        }

        if overview.prop_wash.iter().any(Option::is_some) {
            ui.add_space(12.0);
            ui.heading(t!("Сбросы газа (prop wash)", "Throttle chops (prop wash)"));
            ui.colored_label(theme::MUTED, t!("Колебания ошибки слежения в полосе 20–90 Гц за 0.3 с после сброса газа — во сколько раз сильнее обычного на этом участке.", "Tracking-error oscillation in the 20–90 Hz band within 0.3 s after a throttle chop — how many times stronger than usual in this range."));
            egui::Grid::new("prop_wash").num_columns(3).spacing([24.0, 6.0]).show(ui, |ui| {
                for title in [t!("Ось", "Axis"), t!("Сбросов газа", "Throttle chops"), t!("Колебания после сброса", "Oscillation after a chop")] {
                    ui.colored_label(theme::MUTED, title);
                }
                ui.end_row();
                for (axis, wash) in AXES.iter().zip(&overview.prop_wash) {
                    if let Some(wash) = wash {
                        ui.label(*axis);
                        ui.label(wash.events.to_string());
                        ui.label(tf!("в {:.1} раза сильнее обычного", "{:.1} times stronger than usual", wash.ratio));
                        ui.end_row();
                    }
                }
            });
        }

        if !loaded.log.events.is_empty() {
            ui.add_space(12.0);
            ui.heading(t!("События в логе", "Log events"));
            egui::Grid::new("events").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                for (time, event) in &loaded.log.events {
                    ui.colored_label(theme::MUTED, tf!("{time:.2} с", "{time:.2} s"));
                    ui.label(crate::event_text(event));
                    ui.end_row();
                }
            });
        }
    });
}

/// Параметры из заголовка лога; при сравнении — рядом значения второго лога.
pub fn show_settings(ui: &mut egui::Ui, loaded: &Loaded, compare: Option<&Loaded>, view: &mut View) {
    ui.horizontal(|ui| {
        ui.label(t!("Параметр", "Parameter"));
        ui.add(egui::TextEdit::singleline(&mut view.settings_filter).hint_text(t!("часть названия, например lpf", "part of the name, e.g. lpf")).desired_width(260.0));
        if compare.is_some() {
            ui.checkbox(&mut view.only_different, t!("Только отличающиеся", "Only differing"));
        }
    });
    let needle = view.settings_filter.to_lowercase();
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("settings").num_columns(3).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
            ui.colored_label(theme::MUTED, t!("Параметр", "Parameter"));
            ui.colored_label(theme::MUTED, t!("Этот лог", "This log"));
            if compare.is_some() {
                ui.colored_label(theme::MUTED, t!("Лог сравнения", "Comparison log"));
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
                    ui.colored_label(if differs { theme::WARN } else { theme::MUTED }, other.unwrap_or(t!("нет в логе", "not in the log")));
                }
                ui.end_row();
            }
        });
    });
}

//! Спектральный анализ.

use realfft::RealFftPlanner;

/// Матрица уровней в дБ: `db[x * h + y]`, где `y` — номер частотной полосы.
/// `NaN` — данных нет.
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub db: Vec<f32>,
}

fn to_db(power: f32) -> f32 {
    10.0 * power.max(1e-12).log10()
}

/// Наибольшая степень двойки, не превышающая `limit` (но не меньше 64).
fn pow2_at_most(limit: f64) -> usize {
    let mut n = 64;
    while (n * 2) as f64 <= limit {
        n *= 2;
    }
    n
}

/// Спектральная плотность мощности по окнам длиной `n` с шагом `hop`
/// (окно Ханна, среднее в окне вычтено). Результат — `кадры × (n/2+1)`.
fn stft(x: &[f32], fs: f64, n: usize, hop: usize) -> Vec<f32> {
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let scale = (2.0 / (fs * window.iter().map(|&w| (w * w) as f64).sum::<f64>())) as f32;
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let (mut buf, mut out) = (fft.make_input_vec(), fft.make_output_vec());
    let mut power = Vec::new();
    for seg in x.windows(n).step_by(hop) {
        let mean = seg.iter().sum::<f32>() / n as f32;
        for ((b, &s), &w) in buf.iter_mut().zip(seg).zip(&window) {
            *b = (s - mean) * w;
        }
        fft.process(&mut buf, &mut out).unwrap();
        power.extend(out.iter().map(|c| c.norm_sqr() * scale));
    }
    power
}

/// Спектр по Уэлчу (окно около секунды, перекрытие 50%).
/// Возвращает точки `[частота, Гц; уровень, дБ]`.
pub fn welch_db(x: &[f32], fs: f64) -> Vec<[f64; 2]> {
    let n = pow2_at_most(fs.min(x.len() as f64));
    if x.len() < n || fs <= 0.0 {
        return Vec::new();
    }
    let h = n / 2 + 1;
    let power = stft(x, fs, n, n / 2);
    let frames = (power.len() / h) as f32;
    (1..h) // постоянная составляющая вычтена
        .map(|f| {
            let mean = power.iter().skip(f).step_by(h).sum::<f32>() / frames;
            [f as f64 * fs / n as f64, to_db(mean) as f64]
        })
        .collect()
}

/// Параметры окна для карт: около 0.4 с, не больше 4096 столбцов на лог.
fn map_window(len: usize, fs: f64) -> Option<(usize, usize)> {
    let n = pow2_at_most(fs * 0.4);
    (len >= n && fs > 0.0).then(|| (n, (n / 4).max((len - n) / 4096 + 1)))
}

/// Спектрограмма: по `x` — время (столбцы равномерно покрывают весь сигнал), по `y` — частота.
pub fn spectrogram(x: &[f32], fs: f64) -> Option<Grid> {
    let (n, hop) = map_window(x.len(), fs)?;
    let h = n / 2 + 1;
    let db: Vec<f32> = stft(x, fs, n, hop).into_iter().map(to_db).collect();
    Some(Grid { w: db.len() / h, h, db })
}

/// Шум в зависимости от режима (газа или оборотов): по `x` — режим в процентах
/// своего диапазона, 0..100 (100 столбцов), по `y` — частота.
pub fn binned_heatmap(x: &[f32], key_pct: &[f32], fs: f64) -> Option<Grid> {
    const W: usize = 100;
    let (n, hop) = map_window(x.len(), fs)?;
    let h = n / 2 + 1;
    let (mut sum, mut count) = (vec![0f32; W * h], [0u32; W]);
    for (frame, thr) in stft(x, fs, n, hop).chunks(h).zip(key_pct.windows(n).step_by(hop)) {
        let bin = ((thr.iter().sum::<f32>() / n as f32) as usize).min(W - 1);
        count[bin] += 1;
        for (s, &p) in sum[bin * h..(bin + 1) * h].iter_mut().zip(frame) {
            *s += p;
        }
    }
    // Столбец усредняется вместе с соседними (±1 % диапазона): в одиночном столбце
    // окон мало, и случайное движение стиков выглядело бы как полоса шума.
    let mut db = vec![f32::NAN; W * h];
    for bin in (0..W).filter(|&b| count[b] > 0) {
        let near = bin.saturating_sub(1)..(bin + 2).min(W);
        let frames: u32 = count[near.clone()].iter().sum();
        for f in 0..h {
            db[bin * h + f] = to_db(near.clone().map(|b| sum[b * h + f]).sum::<f32>() / frames as f32);
        }
    }
    Some(Grid { w: W, h, db })
}

/// Отклик на единичную ступеньку и число окон, по которым он получен.
pub struct StepResponse {
    /// Точки `[время, мс; отклик]`; 1.0 — выход сравнялся с заданием.
    pub points: Vec<[f64; 2]>,
    /// Частотная характеристика до 100 Гц: `[частота, Гц; усиление, дБ; фаза, °]`.
    /// Частоты, где оценке нельзя доверять, пропущены.
    pub bode: Vec<[f64; 3]>,
    pub windows: usize,
}

impl StepResponse {
    /// Полоса пропускания: частота, начиная с которой усиление остаётся ниже −3 дБ.
    /// Первый провал не считается: на измеренной кривой он бывает и ниже полосы.
    pub fn bandwidth_hz(&self) -> Option<f64> {
        let last_passing = self.bode.iter().rposition(|p| p[1] >= -3.0)?;
        self.bode.get(last_passing + 1).map(|p| p[0])
    }
}

/// Отклик системы «задание → гироскоп» на ступеньку. В каждом окне (2 с, окно
/// Ханна) оценивается передаточная функция; по окнам берётся медиана на каждой
/// частоте — так окна с ударом или срывом не искажают результат, и отбор окон
/// по «правдоподобию» (который смещает оценку вверх) не нужен. Обратное
/// преобразование даёт импульсную характеристику, её накопленная сумма — отклик.
/// Окна, где стики не двигали, в оценку не входят; если таких окон мало
/// или они плохо согласуются между собой, отклик не выдаётся.
pub fn step_response(setpoint: &[f32], gyro: &[f32], fs: f64) -> Option<StepResponse> {
    const MIN_INPUT: f32 = 20.0; // град/с
    const MIN_TRUST: f32 = 0.5;
    const MIN_WINDOWS: usize = 30; // окна идут через 0.5 с: нужно от 15 с движений стиков
    let n = (fs * 2.0) as usize & !1;
    let len = (fs * 0.5) as usize;
    if n < 64 || setpoint.len() < n {
        return None;
    }
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let mut planner = RealFftPlanner::<f32>::new();
    let (fft, ifft) = (planner.plan_fft_forward(n), planner.plan_fft_inverse(n));
    let (mut buf, mut a, mut b) = (fft.make_input_vec(), fft.make_output_vec(), fft.make_output_vec());
    let bins = a.len();
    // Оценки по окнам: `re[частота]` и `im[частота]` — значения по всем окнам.
    let (mut re, mut im) = (vec![Vec::new(); bins], vec![Vec::new(); bins]);
    for (x, y) in setpoint.windows(n).zip(gyro.windows(n)).step_by(n / 4) {
        if x.iter().fold(0f32, |m, v| m.max(v.abs())) < MIN_INPUT {
            continue;
        }
        for (signal, spectrum) in [(x, &mut a), (y, &mut b)] {
            for ((v, &s), &w) in buf.iter_mut().zip(signal).zip(&window) {
                *v = s * w;
            }
            fft.process(&mut buf, spectrum).unwrap();
        }
        let floor = a.iter().map(|c| c.norm_sqr()).sum::<f32>() / bins as f32 * 1e-4;
        for i in 0..bins {
            let h = b[i] * a[i].conj() / (a[i].norm_sqr() + floor);
            re[i].push(h.re);
            im[i].push(h.im);
        }
    }
    let windows = re[0].len();
    if windows < MIN_WINDOWS {
        return None; // по нескольким окнам медиана — сама шум
    }
    // Медиана и квадрат её стандартной ошибки (через медианное абсолютное отклонение).
    let median = |v: &mut Vec<f32>| {
        let m = *v.select_nth_unstable_by(windows / 2, f32::total_cmp).1;
        v.iter_mut().for_each(|x| *x = (*x - m).abs());
        let sigma = 1.4826 * *v.select_nth_unstable_by(windows / 2, f32::total_cmp).1;
        (m, sigma * sigma * std::f32::consts::FRAC_PI_2 / windows as f32)
    };
    // Доверие к оценке на частотах 1–20 Гц, где и формируется отклик.
    let (mut trust, mut trusted) = (0.0, 0);
    let mut weighted = vec![(realfft::num_complex::Complex::new(0f32, 0f32), 0f32); bins];
    for i in 0..bins {
        let ((m_re, var_re), (m_im, var_im)) = (median(&mut re[i]), median(&mut im[i]));
        // Там, где окна расходятся между собой сильнее самой оценки, она стягивается к нулю:
        // шум на частотах без движений стиков не превращается в ложные выбросы отклика.
        let (signal, noise) = (m_re * m_re + m_im * m_im, var_re + var_im);
        let weight = if signal > 0.0 { signal / (signal + noise) } else { 0.0 };
        a[i] = realfft::num_complex::Complex::new(m_re, m_im) * weight;
        let hz = i as f64 * fs / n as f64;
        if (1.0..=20.0).contains(&hz) {
            trust += weight;
            trusted += 1;
        }
        weighted[i] = (realfft::num_complex::Complex::new(m_re, m_im) * weight, weight);
    }
    if trust / (trusted as f32) < MIN_TRUST {
        return None;
    }
    // Частотная характеристика: 40 точек от 1 до 100 Гц с равным шагом по логарифму.
    // Каждая — среднее оценок в полосе ±12 % с весами доверия: соседние частоты
    // сглаживают друг друга, а ненадёжные почти не участвуют.
    let (mut bode, mut unwrapped) = (Vec::new(), 0.0);
    for k in 0..40 {
        let hz = 100f64.powf(k as f64 / 39.0);
        let band = |edge: f64| ((edge * n as f64 / fs).round() as usize).min(bins - 1);
        let (lo, hi) = (band(hz / 1.12).max(1), band(hz * 1.12));
        if hz > fs * 0.4 || lo > hi {
            continue;
        }
        let (sum, weight) = weighted[lo..=hi].iter().fold((realfft::num_complex::Complex::new(0f32, 0f32), 0f32), |acc, w| (acc.0 + w.0, acc.1 + w.1));
        if weight / (hi - lo + 1) as f32 > 0.3 {
            let mean = sum / weight;
            // Фаза разворачивается: скачки на ±360° убираются относительно предыдущей точки.
            let phase = (mean.im.atan2(mean.re) as f64).to_degrees();
            unwrapped += (phase - unwrapped + 180.0).rem_euclid(360.0) - 180.0;
            bode.push([hz, 20.0 * (mean.norm() as f64).log10(), unwrapped]);
        }
    }
    a[bins - 1].im = 0.0; // частота Найквиста у вещественного сигнала вещественна
    a[0].im = 0.0;
    ifft.process(&mut a, &mut buf).unwrap();
    let mut acc = 0.0;
    let points = buf[..len]
        .iter()
        .enumerate()
        .map(|(i, v)| {
            acc += (v / n as f32) as f64; // обратное БПФ не нормировано
            [i as f64 * 1000.0 / fs, acc]
        })
        .collect();
    Some(StepResponse { points, bode, windows })
}

/// Задержка `filtered` относительно `raw` на низких частотах, мс — групповая
/// задержка фильтра. Берётся из наклона фазы взаимного спектра в полосе
/// 5–80 Гц: сдвиг на время τ даёт фазу −2π·f·τ.
pub fn delay_ms(raw: &[f32], filtered: &[f32], fs: f64) -> Option<f64> {
    let n = pow2_at_most(fs / 2.0);
    let len = raw.len().min(filtered.len());
    if len < 8 * n {
        return None;
    }
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let (mut buf, mut a, mut b) = (fft.make_input_vec(), fft.make_output_vec(), fft.make_output_vec());
    let mut cross = vec![realfft::num_complex::Complex::new(0f64, 0f64); a.len()];
    for (x, y) in raw[..len].windows(n).zip(filtered[..len].windows(n)).step_by(n / 2) {
        for (signal, spectrum) in [(x, &mut a), (y, &mut b)] {
            let mean = signal.iter().sum::<f32>() / n as f32;
            for ((v, &s), &w) in buf.iter_mut().zip(signal).zip(&window) {
                *v = (s - mean) * w;
            }
            fft.process(&mut buf, spectrum).unwrap();
        }
        for (c, (a, b)) in cross.iter_mut().zip(a.iter().zip(&b)) {
            let product = b * a.conj();
            *c += realfft::num_complex::Complex::new(product.re as f64, product.im as f64);
        }
    }
    // Взвешенная прямая через ноль: φ(f) = −2π·f·τ, вес — мощность на частоте.
    let (mut num, mut den) = (0.0, 0.0);
    for (i, c) in cross.iter().enumerate() {
        let hz = i as f64 * fs / n as f64;
        if (5.0..=80f64.min(fs / 8.0)).contains(&hz) {
            num += c.norm() * hz * c.arg();
            den += c.norm() * hz * hz;
        }
    }
    (den > 0.0).then(|| (-num / (std::f64::consts::TAU * den) * 1000.0).max(0.0))
}

/// Фильтр нижних частот PT1 (как в Betaflight), применённый `order` раз подряд:
/// 1 — PT1, 2 — PT2, 3 — PT3. Частота среза каскада по уровню −3 дБ равна `cutoff_hz`.
pub fn lowpass(x: &[f32], cutoff_hz: f64, order: u32, fs: f64) -> Vec<f32> {
    // У каскада одинаковых звеньев общий срез ниже среза звена — поправка как в Betaflight.
    let stage_hz = cutoff_hz / (2f64.powf(1.0 / order as f64) - 1.0).sqrt();
    let rc = 1.0 / (std::f64::consts::TAU * stage_hz);
    let k = ((1.0 / fs) / (rc + 1.0 / fs)) as f32;
    let mut out = x.to_vec();
    for _ in 0..order {
        let mut state = out.first().copied().unwrap_or(0.0);
        for v in &mut out {
            state += k * (*v - state);
            *v = state;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 1000.0;

    fn sine(hz: f32, len: usize) -> Vec<f32> {
        (0..len).map(|i| (std::f32::consts::TAU * hz * i as f32 / FS as f32).sin()).collect()
    }

    /// Частота самой мощной полосы столбца `x`.
    fn peak_hz(g: &Grid, x: usize) -> f64 {
        let col = &g.db[x * g.h..(x + 1) * g.h];
        let f = (0..g.h).max_by(|&a, &b| col[a].total_cmp(&col[b])).unwrap();
        f as f64 * FS / 2.0 / (g.h - 1) as f64
    }

    #[test]
    fn sine_peak_at_its_frequency() {
        let psd = welch_db(&sine(120.0, 8000), FS);
        let peak = psd.iter().max_by(|a, b| a[1].total_cmp(&b[1])).unwrap();
        assert!((peak[0] - 120.0).abs() < 2.0, "пик на {} Гц", peak[0]);
    }

    #[test]
    fn spectrogram_follows_frequency_change() {
        let mut x = sine(100.0, 5000);
        x.extend(sine(300.0, 5000));
        let g = spectrogram(&x, FS).unwrap();
        assert!((peak_hz(&g, g.w / 4) - 100.0).abs() < 5.0);
        assert!((peak_hz(&g, 3 * g.w / 4) - 300.0).abs() < 5.0);
    }

    #[test]
    fn heatmap_bins_by_throttle() {
        let mut x = sine(100.0, 5000);
        x.extend(sine(300.0, 5000));
        let throttle: Vec<f32> = (0..10000).map(|i| if i < 5000 { 20.5 } else { 70.5 }).collect();
        let g = binned_heatmap(&x, &throttle, FS).unwrap();
        assert!((peak_hz(&g, 20) - 100.0).abs() < 5.0);
        assert!((peak_hz(&g, 70) - 300.0).abs() < 5.0);
        assert!(g.db[50 * g.h].is_nan(), "при газе 50 % данных не было");
    }

    #[test]
    fn step_response_of_first_order_lag() {
        // Задание — случайные ступеньки, «гироскоп» — апериодическое звено с τ = 20 мс.
        let tau = 0.02;
        let (mut seed, mut level, mut y) = (1u32, 0f32, 0f32);
        let (mut setpoint, mut gyro) = (Vec::new(), Vec::new());
        for i in 0..60_000 {
            if i % 300 == 0 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                level = (seed >> 16) as f32 / 65536.0 * 600.0 - 300.0;
            }
            y += (level - y) / (tau * FS as f32);
            setpoint.push(level);
            gyro.push(y);
        }
        let r = step_response(&setpoint, &gyro, FS).unwrap();
        let at = |ms: f64| r.points.iter().find(|p| p[0] >= ms).unwrap()[1];
        assert!((at(20.0) - 0.63).abs() < 0.08, "через τ отклик {}", at(20.0));
        assert!((at(200.0) - 1.0).abs() < 0.05, "установившийся отклик {}", at(200.0));
        // Срез звена 1/(2πτ) ≈ 8 Гц: там усиление −3 дБ и фаза −45°.
        let near = r.bode.iter().min_by(|a, b| (a[0] - 8.0).abs().total_cmp(&(b[0] - 8.0).abs())).unwrap();
        assert!((near[1] + 3.0).abs() < 1.0 && (near[2] + 45.0).abs() < 8.0, "на {} Гц: {} дБ, {}°", near[0], near[1], near[2]);
        assert!((r.bandwidth_hz().unwrap() - 8.0).abs() < 1.5);
    }

    /// Шум без повторов — для проверки задержек и фильтров.
    fn noise(len: usize) -> Vec<f32> {
        let mut seed = 3u32;
        (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn delay_of_shifted_signal() {
        let raw = noise(20_000);
        let mut delayed = vec![0.0; 3];
        delayed.extend(&raw[..raw.len() - 3]);
        assert!((delay_ms(&raw, &delayed, FS).unwrap() - 3.0).abs() < 0.1);
    }

    #[test]
    fn delay_of_pt1_is_its_time_constant() {
        // У PT1 задержка на низких частотах равна постоянной времени 1/(2π·fc): 1.59 мс при 100 Гц.
        let fs = 8000.0;
        let raw = noise(80_000);
        let delay = delay_ms(&raw, &lowpass(&raw, 100.0, 1, fs), fs).unwrap();
        assert!((delay - 1.59).abs() < 0.2, "{delay} мс");
    }

    #[test]
    fn lowpass_cutoff_is_minus_3_db() {
        for order in 1..=3 {
            let (raw, filtered) = (sine(10.0, 20_000), lowpass(&sine(10.0, 20_000), 10.0, order, FS));
            let rms = |x: &[f32]| (x[10_000..].iter().map(|v| v * v).sum::<f32>() / 10_000.0).sqrt();
            let db = 20.0 * (rms(&filtered) / rms(&raw)).log10();
            assert!((db + 3.0).abs() < 0.5, "порядок {order}: {db} дБ");
        }
    }
}

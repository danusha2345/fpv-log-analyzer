//! Разбор blackbox-логов Betaflight/INAV (data version 2).
//!
//! Поля берутся по именам из заголовка, без привязки к версии прошивки.
//! Алгоритм декодирования повторяет `blackbox_decode` (parser.c / decoders.c).

const LOG_MARKER: &[u8] = b"H Product:Blackbox flight data recorder by Nicholas Sherlock\n";

pub struct Log {
    pub headers: Vec<(String, String)>,
    /// Имена полей основного кадра (I/P), в порядке заголовка.
    pub names: Vec<String>,
    /// Значения полей по столбцам: `cols[поле][кадр]`.
    pub cols: Vec<Vec<f32>>,
    /// Время кадра в секундах от начала лога.
    pub time: Vec<f64>,
    /// Сколько в записи повреждённых участков (разрывов), где кадры не читаются.
    pub corrupt: usize,
    /// События лога: `(время, с; описание)`.
    pub events: Vec<(f64, String)>,
}

impl Log {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn col(&self, name: &str) -> Option<&[f32]> {
        let i = self.names.iter().position(|n| n == name)?;
        Some(&self.cols[i])
    }

    /// Частота записи кадров, Гц (по медиане шага времени).
    pub fn sample_rate(&self) -> f64 {
        let mut dt: Vec<f64> = self.time.windows(2).map(|w| w[1] - w[0]).collect();
        if dt.is_empty() {
            return 0.0;
        }
        let mid = dt.len() / 2;
        let (_, m, _) = dt.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        1.0 / *m
    }
}

/// Все логи из файла (в одном .bbl их может быть несколько).
pub fn parse_file(data: &[u8]) -> Vec<Result<Log, String>> {
    let mut starts: Vec<usize> = data
        .windows(LOG_MARKER.len())
        .enumerate()
        .filter(|(_, w)| *w == LOG_MARKER)
        .map(|(i, _)| i)
        .collect();
    starts.push(data.len());
    starts.windows(2).map(|w| parse_log(&data[w[0]..w[1]])).collect()
}

#[derive(Default)]
struct FrameDef {
    signed: Vec<bool>,
    predictor: Vec<u8>,
    encoding: Vec<u8>,
}

#[derive(Default)]
struct Ctx {
    min_throttle: i32,
    vbat_ref: i32,
    min_motor: i32,
    motor0: Option<usize>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Чтение вышло за конец данных.
    overrun: bool,
}

impl Reader<'_> {
    fn byte(&mut self) -> u8 {
        match self.data.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b
            }
            None => {
                self.overrun = true;
                0
            }
        }
    }

    fn unsigned_vb(&mut self) -> u32 {
        let mut result = 0u32;
        for shift in (0..35).step_by(7) {
            let b = self.byte();
            result |= ((b & 0x7f) as u32) << shift;
            if b < 0x80 {
                return result;
            }
        }
        self.overrun = true; // больше 5 байт — мусор
        0
    }

    fn signed_vb(&mut self) -> i32 {
        let u = self.unsigned_vb();
        ((u >> 1) as i32) ^ -((u & 1) as i32)
    }
}

fn sign_extend(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

/// Последний вариант TAG2_3S32 / TAG2_3SVARIABLE: размер каждого поля задан отдельно.
fn tag2_3_wide(r: &mut Reader, mut lead: u8, out: &mut [i32; 8]) {
    for v in out.iter_mut().take(3) {
        let bytes = (lead & 3) as u32 + 1;
        let mut raw = 0u32;
        for i in 0..bytes {
            raw |= (r.byte() as u32) << (8 * i);
        }
        *v = sign_extend(raw, 8 * bytes);
        lead >>= 2;
    }
}

fn tag2_3s32(r: &mut Reader, out: &mut [i32; 8]) {
    let lead = r.byte();
    match lead >> 6 {
        0 => {
            out[0] = sign_extend((lead >> 4) as u32 & 3, 2);
            out[1] = sign_extend((lead >> 2) as u32 & 3, 2);
            out[2] = sign_extend(lead as u32 & 3, 2);
        }
        1 => {
            out[0] = sign_extend(lead as u32 & 0x0f, 4);
            let b = r.byte() as u32;
            out[1] = sign_extend(b >> 4, 4);
            out[2] = sign_extend(b & 0x0f, 4);
        }
        2 => {
            out[0] = sign_extend(lead as u32 & 0x3f, 6);
            out[1] = sign_extend(r.byte() as u32 & 0x3f, 6);
            out[2] = sign_extend(r.byte() as u32 & 0x3f, 6);
        }
        _ => tag2_3_wide(r, lead, out),
    }
}

fn tag2_3svariable(r: &mut Reader, out: &mut [i32; 8]) {
    let lead = r.byte() as u32;
    match lead >> 6 {
        0 => {
            out[0] = sign_extend((lead >> 4) & 3, 2);
            out[1] = sign_extend((lead >> 2) & 3, 2);
            out[2] = sign_extend(lead & 3, 2);
        }
        1 => {
            let b2 = r.byte() as u32;
            out[0] = sign_extend((lead & 0x3e) >> 1, 5);
            out[1] = sign_extend(((lead & 1) << 4) | (b2 >> 4), 5);
            out[2] = sign_extend(b2 & 0x0f, 4);
        }
        2 => {
            let b2 = r.byte() as u32;
            let b3 = r.byte() as u32;
            out[0] = sign_extend(((lead & 0x3f) << 2) | (b2 >> 6), 8);
            out[1] = sign_extend(((b2 & 0x3f) << 1) | (b3 >> 7), 7);
            out[2] = sign_extend(b3 & 0x7f, 7);
        }
        _ => tag2_3_wide(r, lead as u8, out),
    }
}

fn tag8_4s16(r: &mut Reader, out: &mut [i32; 8]) {
    let mut selector = r.byte();
    let mut buffer = 0u32;
    let mut half = false; // в buffer остался непрочитанный младший полубайт
    for v in out.iter_mut().take(4) {
        *v = match selector & 3 {
            0 => 0,
            1 => {
                if half {
                    half = false;
                    sign_extend(buffer & 0x0f, 4)
                } else {
                    buffer = r.byte() as u32;
                    half = true;
                    sign_extend(buffer >> 4, 4)
                }
            }
            2 => {
                if half {
                    let hi = (buffer & 0x0f) << 4;
                    buffer = r.byte() as u32;
                    sign_extend(hi | (buffer >> 4), 8)
                } else {
                    sign_extend(r.byte() as u32, 8)
                }
            }
            _ => {
                let (c1, c2) = (r.byte() as u32, r.byte() as u32);
                if half {
                    let v = ((buffer & 0x0f) << 12) | (c1 << 4) | (c2 >> 4);
                    buffer = c2;
                    sign_extend(v, 16)
                } else {
                    sign_extend((c1 << 8) | c2, 16)
                }
            }
        };
        selector >>= 2;
    }
}

/// Декодирует один кадр в `cur`. `skipped` — число намеренно пропущенных
/// итераций перед кадром (нужно предиктору INC).
fn parse_frame(
    r: &mut Reader,
    def: &FrameDef,
    ctx: &Ctx,
    cur: &mut [i32],
    prev: Option<(&[i32], &[i32])>,
    skipped: i32,
) {
    let n = def.encoding.len();
    let mut i = 0;
    while i < n {
        if def.predictor[i] == 6 {
            cur[i] = skipped + 1 + prev.map_or(0, |(p, _)| p[i]);
            i += 1;
            continue;
        }
        let mut values = [0i32; 8];
        let count = match def.encoding[i] {
            0 => {
                values[0] = r.signed_vb();
                1
            }
            1 => {
                values[0] = r.unsigned_vb() as i32;
                1
            }
            3 => {
                values[0] = -sign_extend(r.unsigned_vb(), 14);
                1
            }
            6 => {
                let group = def.encoding[i..n.min(i + 8)].iter().take_while(|&&e| e == 6).count();
                if group == 1 {
                    values[0] = r.signed_vb();
                } else {
                    let mut header = r.byte();
                    for v in values.iter_mut() {
                        if header & 1 != 0 {
                            *v = r.signed_vb();
                        }
                        header >>= 1;
                    }
                }
                group
            }
            7 => {
                tag2_3s32(r, &mut values);
                3
            }
            8 => {
                tag8_4s16(r, &mut values);
                4
            }
            9 => 1,
            10 => {
                tag2_3svariable(r, &mut values);
                3
            }
            _ => {
                r.overrun = true; // неизвестная кодировка — кадр не разобрать
                return;
            }
        };
        for &value in values.iter().take(count.min(n - i)) {
            let base = match def.predictor[i] {
                1 => prev.map_or(0, |(p, _)| p[i]),
                2 => prev.map_or(0, |(p, p2)| p[i].wrapping_mul(2).wrapping_sub(p2[i])),
                3 => prev.map_or(0, |(p, p2)| {
                    if def.signed[i] {
                        ((p[i] as i64 + p2[i] as i64) / 2) as i32
                    } else {
                        ((p[i] as u32 as u64 + p2[i] as u32 as u64) / 2) as i32
                    }
                }),
                4 => ctx.min_throttle,
                5 => ctx.motor0.map_or(0, |m| cur[m]),
                8 => 1500,
                9 => ctx.vbat_ref,
                11 => ctx.min_motor,
                _ => 0, // 0, а также GPS-предикторы 7 и 10: эти кадры только пропускаем
            };
            cur[i] = value.wrapping_add(base);
            i += 1;
        }
    }
}

/// Разбирает событие и возвращает его описание; `None` — событие не распознано.
/// Описание [`END_OF_LOG`] означает конец записи.
fn parse_event(r: &mut Reader, modes: Option<&Modes>) -> Option<String> {
    Some(match r.byte() {
        0 => {
            r.unsigned_vb();
            "Сигнал синхронизации".into()
        }
        13 => {
            if r.byte() & 0x80 != 0 {
                (0..4).for_each(|_| _ = r.byte());
            } else {
                r.signed_vb();
            }
            "Подстройка в полёте".into()
        }
        14 => {
            r.unsigned_vb();
            r.unsigned_vb();
            "Запись возобновлена".into()
        }
        15 => format!("Дизарм, причина {}", r.unsigned_vb()),
        30 => {
            let (now, before) = (r.unsigned_vb(), r.unsigned_vb());
            format!("Режимы полёта: {} → {}", mode_names(before, modes), mode_names(now, modes))
        }
        255 => {
            const END: &[u8] = b"End of log\0";
            if !r.data[r.pos..].starts_with(END) {
                return None;
            }
            END_OF_LOG.into()
        }
        _ => return None,
    })
}

const END_OF_LOG: &str = "Конец записи";

/// Раскладки битов режимов полёта — порядок `boxId` в `src/main/fc/rc_modes.h`
/// соответствующих версий Betaflight.
type Modes = [&'static str; 32];
const MODES_4: Modes = [
    "ARM", "ANGLE", "HORIZON", "MAG", "HEADFREE", "PASSTHRU", "FAILSAFE", "GPSRESCUE", "ANTIGRAVITY", "HEADADJ", "CAMSTAB", "BEEPERON", "LEDLOW", "CALIB", "OSD", "TELEMETRY",
    "SERVO1", "SERVO2", "SERVO3", "BLACKBOX", "AIRMODE", "3D", "FPVANGLEMIX", "BLACKBOXERASE", "CAMERA1", "CAMERA2", "CAMERA3", "FLIPOVERAFTERCRASH", "PREARM", "BEEPGPSCOUNT", "VTXPITMODE", "PARALYZE",
];
const MODES_2025: Modes = [
    "ARM", "ANGLE", "HORIZON", "MAG", "ALTHOLD", "HEADFREE", "CHIRP", "PASSTHRU", "FAILSAFE", "POSHOLD", "GPSRESCUE", "ANTIGRAVITY", "HEADADJ", "CAMSTAB", "BEEPERON", "LEDLOW",
    "CALIB", "OSD", "TELEMETRY", "SERVO1", "SERVO2", "SERVO3", "BLACKBOX", "AIRMODE", "3D", "FPVANGLEMIX", "BLACKBOXERASE", "CAMERA1", "CAMERA2", "CAMERA3", "CRASHFLIP", "PREARM",
];
const MODES_2026: Modes = [
    "ARM", "ANGLE", "HORIZON", "MAG", "ALTHOLD", "HEADFREE", "CHIRP", "PASSTHRU", "FAILSAFE", "POSHOLD", "GPSRESCUE", "AUTOPILOT", "ANTIGRAVITY", "HEADADJ", "CAMSTAB", "BEEPERON",
    "LEDLOW", "CALIB", "OSD", "TELEMETRY", "SERVO1", "SERVO2", "SERVO3", "BLACKBOX", "AIRMODE", "3D", "FPVANGLEMIX", "BLACKBOXERASE", "CAMERA1", "CAMERA2", "CAMERA3", "CRASHFLIP",
];

/// Раскладка для прошивки, если она известна (сверено с исходниками версий
/// 4.2.11, 4.3.2, 4.4.3, 4.5.2, 2025.12.2 и 2026.x).
fn modes_of(firmware: &str) -> Option<&'static Modes> {
    let version = firmware.strip_prefix("Betaflight ")?;
    match version {
        v if ["4.2.", "4.3.", "4.4.", "4.5."].iter().any(|p| v.starts_with(p)) => Some(&MODES_4),
        v if v.starts_with("2025.12") => Some(&MODES_2025),
        v if v.starts_with("2026.") => Some(&MODES_2026),
        _ => None,
    }
}

/// Включённые режимы полёта по битовой маске. Первые три бита одинаковы во всех
/// прошивках; остальные названы только там, где раскладка известна, иначе показан номер бита.
fn mode_names(mask: u32, modes: Option<&Modes>) -> String {
    let names: Vec<String> = (0..32usize)
        .filter(|bit| mask >> bit & 1 != 0)
        .map(|bit| match modes {
            Some(modes) => modes[bit].to_string(),
            None if bit < 3 => MODES_4[bit].to_string(),
            None => format!("бит {bit}"),
        })
        .collect();
    if names.is_empty() { "нет".into() } else { names.join(", ") }
}

fn parse_list<T: std::str::FromStr>(value: &str) -> Vec<T> {
    value.split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

fn parse_log(data: &[u8]) -> Result<Log, String> {
    let mut headers = Vec::new();
    let mut pos = 0;
    while data[pos..].starts_with(b"H ") {
        let end = data[pos..].iter().position(|&b| b == b'\n').ok_or("заголовок оборван")?;
        let line = String::from_utf8_lossy(&data[pos + 2..pos + end]);
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.to_string(), v.to_string()));
        }
        pos += end + 1;
    }
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    let def = |kind: char, names_from: char| -> FrameDef {
        let field = |frame: char, what: &str| get(&format!("Field {frame} {what}")).unwrap_or("");
        FrameDef {
            signed: parse_list::<u8>(field(names_from, "signed")).iter().map(|&s| s != 0).collect(),
            predictor: parse_list(field(kind, "predictor")),
            encoding: parse_list(field(kind, "encoding")),
        }
    };
    let names: Vec<String> = get("Field I name").ok_or("нет заголовка Field I name")?.split(',').map(str::to_string).collect();
    let (def_i, def_p) = (def('I', 'I'), def('P', 'I'));
    let (def_s, def_g, def_h) = (def('S', 'S'), def('G', 'G'), def('H', 'H'));
    let n = names.len();
    for (d, kind) in [(&def_i, 'I'), (&def_p, 'P')] {
        if d.encoding.len() != n || d.predictor.len() != n || d.signed.len() != n {
            return Err(format!("заголовок лога повреждён: описание кадра {kind} не совпадает с числом полей ({n})"));
        }
    }
    // Служебные кадры (S, G, H) анализу не нужны — их достаточно уметь пропускать.
    // Если описание такого кадра в заголовке повреждено, кадр считается нечитаемым:
    // разбор пересинхронизируется на следующем, а основной поток данных сохраняется.
    let usable = |d: &FrameDef| d.encoding.len() == d.predictor.len() && d.signed.len() >= d.encoding.len();
    let (s_ok, g_ok, h_ok) = (usable(&def_s), usable(&def_g), usable(&def_h));
    let time_idx = names.iter().position(|s| s == "time").ok_or("нет поля time")?;
    let modes = get("Firmware revision").and_then(modes_of);
    let iter_idx = def_p.predictor.iter().position(|&p| p == 6);

    let num = |name: &str| get(name).and_then(|v| v.trim().parse::<i32>().ok());
    let ctx = Ctx {
        min_throttle: num("minthrottle").unwrap_or(0),
        vbat_ref: num("vbatref").unwrap_or(0),
        min_motor: get("motorOutput").and_then(|v| parse_list::<i32>(v).first().copied()).unwrap_or(0),
        motor0: names.iter().position(|s| s == "motor[0]"),
    };
    let i_interval = num("I interval").unwrap_or(1).max(1);
    let (p_num, p_denom) = match get("P interval").map(|v| v.split_once('/')) {
        Some(Some((a, b))) => (a.trim().parse().unwrap_or(1), b.trim().parse().unwrap_or(1)),
        _ => (1, num("P interval").unwrap_or(1)),
    };
    let p_denom = p_denom.max(1);
    let should_have = |iter: i32| (iter.rem_euclid(i_interval) + p_num - 1) % p_denom < p_num;

    let mut log = Log { headers: headers.clone(), names, cols: vec![Vec::new(); n], time: Vec::new(), corrupt: 0, events: Vec::new() };
    let mut r = Reader { data, pos, overrun: false };
    let (mut cur, mut prev, mut prev2) = (vec![0i32; n], vec![0i32; n], vec![0i32; n]);
    let mut scratch = vec![0i32; def_s.encoding.len().max(def_g.encoding.len()).max(def_h.encoding.len())];
    let mut stream_valid = false; // можно ли опираться на prev для P-кадров
    let mut in_garbage = false; // идёт повреждённый участок
    let (mut last_time, mut time_hi, mut t0) = (0u32, 0u64, None);
    let mut last_us = 0u64;

    while r.pos < data.len() {
        let start = r.pos;
        r.overrun = false;
        let kind = r.byte();
        let mut event = None;
        let mut ok = match kind {
            b'I' => {
                parse_frame(&mut r, &def_i, &ctx, &mut cur, None, 0);
                true
            }
            b'P' => {
                let skipped = iter_idx.map_or(0, |i| {
                    let first = prev[i].wrapping_add(1);
                    (first..first.saturating_add(4096)).take_while(|&it| !should_have(it)).count() as i32
                });
                parse_frame(&mut r, &def_p, &ctx, &mut cur, Some((&prev, &prev2)), skipped);
                true
            }
            b'S' | b'G' | b'H' => {
                let (def, usable) = match kind {
                    b'S' => (&def_s, s_ok),
                    b'G' => (&def_g, g_ok),
                    _ => (&def_h, h_ok),
                };
                if usable {
                    parse_frame(&mut r, def, &ctx, &mut scratch, None, 0);
                }
                usable
            }
            b'E' => {
                event = parse_event(&mut r, modes);
                event.is_some()
            }
            _ => false,
        };
        // Кадр настоящий, только если сразу за ним начинается следующий (или конец данных).
        let end_of_log = event.as_deref() == Some(END_OF_LOG);
        ok &= !r.overrun && (end_of_log || data.get(r.pos).is_none_or(|b| b"IPSGHE".contains(b)));

        if ok && (kind == b'I' || (kind == b'P' && stream_valid)) {
            let raw = cur[time_idx] as u32;
            let mut hi = time_hi;
            if raw < last_time && last_time - raw > 1 << 31 {
                hi += 1 << 32; // переполнение 32-битного счётчика микросекунд
            }
            let us = hi + raw as u64;
            // Время не идёт назад, а между соседними P-кадрами нет скачков.
            if us < last_us || (kind == b'P' && us - last_us > 10_000_000) {
                ok = false;
            } else {
                (time_hi, last_time, last_us) = (hi, raw, us);
                let t0 = *t0.get_or_insert(us);
                log.time.push((us - t0) as f64 / 1e6);
                for (i, col) in log.cols.iter_mut().enumerate() {
                    col.push(if def_i.signed[i] { cur[i] as f32 } else { cur[i] as u32 as f32 });
                }
                if kind == b'I' {
                    prev2.copy_from_slice(&cur);
                } else {
                    std::mem::swap(&mut prev, &mut prev2);
                }
                prev.copy_from_slice(&cur);
                stream_valid = true;
            }
        }
        if let (true, Some(event)) = (ok, event) {
            log.events.push((log.time.last().copied().unwrap_or(0.0), event));
        }
        if !ok {
            // Внутри повреждённого участка случайные байты то и дело похожи на начало
            // кадра — участок считается один раз, а не по числу таких совпадений.
            if !in_garbage {
                log.corrupt += 1;
            }
            stream_valid &= !matches!(kind, b'I' | b'P');
            r.pos = start + 1;
        }
        in_garbage = !ok;
        if end_of_log {
            break;
        }
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    #[test]
    fn flight_mode_names_follow_firmware_version() {
        use super::{mode_names, modes_of};
        // ARM и AIRMODE: бит 20 в Betaflight 4.x, 23 в 2025.12, 24 в 2026.x.
        assert_eq!(mode_names(1 | 1 << 20, modes_of("Betaflight 4.5.0 (5cdae1b0c) STM32F7X2")), "ARM, AIRMODE");
        assert_eq!(mode_names(1 | 1 << 23, modes_of("Betaflight 2025.12.2 (79065c96b) AT32F435M")), "ARM, AIRMODE");
        assert_eq!(mode_names(1 | 1 << 24, modes_of("Betaflight 2026.6.0-alpha (c40f1e096) STM32G47X")), "ARM, AIRMODE");
        // Для неизвестной раскладки названы только общие первые три бита.
        assert_eq!(mode_names(0b11 | 1 << 9, modes_of("INAV 9.0.1 (d44f2cf6) DAKEFPVF722")), "ARM, ANGLE, бит 9");
        assert_eq!(mode_names(0, None), "нет");
    }

    /// Повреждённые и оборванные файлы не должны ронять разбор.
    #[test]
    fn damaged_files_do_not_panic() {
        let path = std::env::var("HOME").unwrap() + "/storage/adrc-8ksal8-goodtune/ADRC_Good_Tune.bbl";
        let Ok(mut data) = std::fs::read(path) else { return };
        data.truncate(400_000);
        let mut seed = 7u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as usize
        };
        for _ in 0..40 {
            let mut damaged = data[..random() % data.len()].to_vec();
            for _ in 0..200 {
                let at = random() % damaged.len().max(1);
                if let Some(b) = damaged.get_mut(at) {
                    *b = random() as u8;
                }
            }
            for log in super::parse_file(&damaged).into_iter().flatten() {
                assert!(log.cols.iter().all(|c| c.len() == log.time.len()));
            }
        }
    }

    /// Сверка с CSV от `blackbox_decode`; пропускается, если эталона нет на диске.
    #[test]
    fn matches_blackbox_decode() {
        let dir = std::env::var("HOME").unwrap() + "/storage/adrc-8ksal8-goodtune/";
        let (Ok(bbl), Ok(csv)) = (std::fs::read(dir.clone() + "ADRC_Good_Tune.bbl"), std::fs::read_to_string(dir + "ADRC_Good_Tune.01.csv")) else {
            return;
        };
        let log = super::parse_file(&bbl).remove(0).unwrap();
        let mut lines = csv.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').map(str::trim).collect();
        let rows: Vec<&str> = lines.collect();
        // Эталонный декодер старый: не понимает целочисленный `P interval`, поэтому
        // неверно считает loopIteration и теряет часть исправных кадров. Сверяем по времени.
        let us = |cells: &[&str]| cells[1].parse::<f64>().unwrap();
        let t0 = us(&rows[0].split(',').map(str::trim).collect::<Vec<_>>());
        let mut mine = 0;
        for line in &rows {
            let cells: Vec<&str> = line.split(',').map(str::trim).collect();
            let t = us(&cells) - t0;
            while (log.time[mine] * 1e6).round() < t {
                mine += 1;
            }
            assert_eq!((log.time[mine] * 1e6).round(), t);
            for (i, name) in log.names.iter().enumerate() {
                // Столбцы с единицами измерения ("time (us)", "vbatLatest (V)") decode масштабирует.
                if header[i] != name || name == "loopIteration" {
                    continue;
                }
                let want: f32 = cells[i].parse().unwrap();
                assert_eq!(log.cols[i][mine], want, "t={t} мкс, поле {name}");
            }
        }
        assert!(log.time.len() - rows.len() < 100);
    }
}

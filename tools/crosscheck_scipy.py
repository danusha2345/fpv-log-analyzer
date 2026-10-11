"""Сверка анализа с независимыми расчётами на scipy.

Запуск: python3 tools/crosscheck_scipy.py <лог> [путь к fpv-log-analyzer]
Берёт из программы выгрузки `csv` и `response` и сравнивает:
спектр gyroADC — со scipy.signal.welch; задержку фильтров — с наклоном фазы
взаимного спектра; частотную характеристику — с оценкой H1 = Pxy/Pxx;
отклик на ступеньку — с подбором импульсной характеристики методом наименьших квадратов.
"""
import collections, io, subprocess, sys
import numpy as np
from scipy import signal

log = sys.argv[1]
exe = sys.argv[2] if len(sys.argv) > 2 else 'target/release/fpv-log-analyzer'
run = lambda mode: subprocess.run([exe, log, mode], capture_output=True, text=True).stdout
text = run('csv'); names = text[:text.index('\n')].split(',')
want = [f'{b}[{a}]' for b in ('setpoint', 'gyroADC', 'gyroUnfilt') for a in range(3) if f'{b}[{a}]' in names]
d = np.loadtxt(io.StringIO(text), delimiter=',', skiprows=1, usecols=[0] + [names.index(w) for w in want])
col = lambda name: d[:, 1 + want.index(name)] if name in want else None
fs = 1e6 / np.median(np.diff(d[:, 0]))
resp = collections.defaultdict(list)
for line in run('response').splitlines():
    kind, *v = line.split(','); resp[kind].append(v)
print(f'{log}: {len(d)} кадров, {fs:.0f} Гц')
for a, ax in enumerate(['Roll', 'Pitch', 'Yaw']):
    sp, gy, raw = col(f'setpoint[{a}]'), col(f'gyroADC[{a}]'), col(f'gyroUnfilt[{a}]')
    print(ax)
    psd = np.array([[float(x) for x in v[2:]] for v in resp['psd'] if v[0] == 'gyroADC' and v[1] == ax])
    if len(psd) > 2:
        n = int(round(fs / (psd[1, 0] - psd[0, 0])))
        f, p = signal.welch(gy, fs=fs, nperseg=n, noverlap=n // 2)
        diff = psd[:, 1] - np.interp(psd[:, 0], f, 10 * np.log10(p))
        print(f'  спектр: расхождение среднее {diff.mean():+.3f} дБ, наибольшее {abs(diff).max():.3f} дБ')
    ours = [float(v[1]) for v in resp['delay'] if v[0] == ax]
    if ours and raw is not None:
        f, pxy = signal.csd(np.diff(raw), np.diff(gy), fs=fs, nperseg=1024, noverlap=512)
        b = (f >= 5) & (f <= 80); w = abs(pxy[b])
        ref = -(w * f[b] * np.angle(pxy[b])).sum() / (2 * np.pi * (w * f[b] ** 2).sum()) * 1000
        print(f'  задержка фильтров: у нас {ours[0]:.2f} мс, scipy {ref:.2f} мс')
    bode = np.array([[float(x) for x in v[1:]] for v in resp['bode'] if v[0] == ax])
    if len(bode) and sp is not None:
        n = int(2 * fs)
        f, pxy = signal.csd(sp, gy, fs=fs, nperseg=n, noverlap=n // 2)
        _, pxx = signal.welch(sp, fs=fs, nperseg=n, noverlap=n // 2); _, pyy = signal.welch(gy, fs=fs, nperseg=n, noverlap=n // 2)
        rows = []
        for hz, db, deg in bode:
            band = (f >= hz / 1.12) & (f <= hz * 1.12)
            if band.any():
                h = (pxy[band] / pxx[band]).mean()
                rows.append((db - 20 * np.log10(abs(h)), deg, np.angle(h), (abs(pxy[band]) ** 2 / (pxx[band] * pyy[band])).mean()))
        r = np.array(rows); good = r[:, 3] > 0.8
        if good.any():
            dphi = r[:, 1] - np.degrees(np.unwrap(r[:, 2]))
            print(f'  частотная характеристика ({good.sum()} точек с когерентностью выше 0.8): усиление {r[good, 0].mean():+.2f} дБ в среднем, до {abs(r[good, 0]).max():.2f} дБ; фаза {dphi[good].mean():+.1f}° в среднем, до {abs(dphi[good]).max():.1f}°')
    step = np.array([[float(x) for x in v[1:]] for v in resp['step'] if v[0] == ax])
    if len(step) and sp is not None:
        q = max(1, int(round(fs / 200))); n = len(sp) // q * q
        x = sp[:n].reshape(-1, q).mean(1); y = gy[:n].reshape(-1, q).mean(1); lags = int(0.6 * fs / q)
        parts = []
        for part in range(3):  # три трети лога — виден разброс самой оценки
            sl = slice(part * len(x) // 3, (part + 1) * len(x) // 3)
            m = np.stack([np.roll(x[sl], k) for k in range(lags)], 1)[lags:]
            parts.append(np.cumsum(np.linalg.lstsq(m, y[sl][lags:], rcond=None)[0]))
        ref = np.mean(parts, 0); spread = np.std(parts, 0); ms = (np.arange(lags) + 1) * 1000 * q / fs
        at = lambda t: step[np.argmin(abs(step[:, 0] - t)), 1]
        print('  отклик (мс: у нас / МНК ± разброс): ' + ', '.join(f'{t}: {at(t):.2f} / {np.interp(t, ms, ref):.2f} ± {np.interp(t, ms, spread):.2f}' for t in (50, 100, 200, 400)))

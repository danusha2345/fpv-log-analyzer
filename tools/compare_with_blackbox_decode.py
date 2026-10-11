"""Сверка разбора с blackbox_decode.

Перед запуском декодировать логи эталоном: рядом с каждым логом должны лежать
его `<имя>.NN.csv`. Запуск: `python3 tools/compare_with_blackbox_decode.py <папка с логами> [путь к fpv-log-analyzer]`.
Сравниваются все поля с одинаковыми именами, кроме времени; строки сопоставляются по времени кадра.
"""
import sys, subprocess, glob, os, io, re
import numpy as np, warnings
warnings.simplefilter('ignore')
B = sys.argv[2] if len(sys.argv) > 2 else 'target/release/fpv-log-analyzer'
logs_dir = sys.argv[1]
class T:
    """Таблица: имена столбцов и числовой массив."""
    def __init__(s, names, data): s.names, s.data = names, data
    def __len__(s): return len(s.data)
    def col(s, name): return s.data[:, s.names.index(name)]
def table(f):
    names = [c.strip() for c in f.readline().split(',')]
    data = np.genfromtxt(f, delimiter=',', invalid_raise=False, usecols=range(len(names)), loose=True)
    return T(names, np.atleast_2d(data) if data.size else np.zeros((0, len(names))))
def mine(path, n):
    out = subprocess.run([B, path, 'csv', str(n)], capture_output=True, text=True).stdout
    if not out.strip(): return None
    return table(io.StringIO(out))
total_bad = 0
for path in sorted(p for p in glob.glob(logs_dir + '/*') if re.search(r'\.(bbl|bfl|txt)$', p, re.I)):
    stem = os.path.splitext(path)[0]
    fw = ''
    with open(path, 'rb') as f:
        m = re.search(rb'H Firmware revision:([^\n]{0,28})', f.read(20000)); fw = m.group(1).decode(errors='replace') if m else '?'
    refs = []
    for c in sorted(glob.glob(glob.escape(stem) + '.[0-9][0-9].csv')):
        try:
            with open(c) as f: r = table(f)
        except Exception as e: continue
        if len(r) >= 3: refs.append((os.path.basename(c)[-6:-4], r))
    mines = []
    n = 1
    while (m := mine(path, n)) is not None: mines.append(m); n += 1
    print(f'== {os.path.basename(path)} [{fw}] эталон: {len(refs)} записей с данными, у нас: {len(mines)}')
    used = set()
    for tag, r in refs:
        tcol = next(c for c in r.names if c.startswith('time'))
        rt = r.col(tcol)
        best = None
        for k, m in enumerate(mines):
            if k in used: continue
            mt = m.col('time_us')
            # смещение: абсолютное время эталона минус наше относительное; подбираем по первым кадрам
            # эталон иногда пропускает начало записи (или мы) — пробуем оба варианта
            cands = {rt[j] - mt[0]: 1 for j in range(min(600, len(rt)))}
            cands.update({rt[0] - mt[i]: 1 for i in range(min(600, len(mt)))})
            for off, _ in cands.items():
                hit = np.isin(mt[:1500] + off, rt[:1500]).sum()
                if best is None or hit > best[0]: best = (hit, k, off)
        if best is None or best[0] < 3:
            print(f'   запись {tag}: {len(r)} кадров — у нас пары нет'); total_bad += 1; continue
        _, k, off = best; used.add(k); m = mines[k]
        mt = m.col('time_us') + off
        common, ri, mi = np.intersect1d(rt, mt, return_indices=True)
        names = [n for n in r.names if n in m.names and n != 'time']
        bad = {}
        for nme in names:
            d = int((np.abs(r.col(nme)[ri] - m.col(nme)[mi]) > 0.5).sum())
            if d: bad[nme] = d
        it = bad.pop('loopIteration', 0)
        worst = ', '.join(f'{k}:{v}' for k, v in sorted(bad.items(), key=lambda kv: -kv[1])[:4])
        if bad: total_bad += 1
        print(f'   запись {tag}: эталон {len(r)}, у нас {len(m)}, общих {len(common)}, полей {len(names)}, расхождений в полях: {len(bad)} {worst}' + (f' | loopIteration расходится в {it}' if it else ''))
print('ИТОГО записей с расхождениями или без пары:', total_bad)

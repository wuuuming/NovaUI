#!/usr/bin/env python3
"""End-to-end check of the gallery example on a real X11 display.

Runs the gallery with NOVAUI_GALLERY_OUT, and whenever it prints `READY <n> <window> WxH`
captures that window from the X server (ImageMagick `import`, i.e. what the compositor shows)
and compares it with the software reference the app wrote for exactly that tree state.
Pass: every channel within the GPU tolerance (2/255, at most 16 px at 3) for every phase.
Usage: gallery_e2e.py <gallery-binary> <out-dir> [backend]
"""
import subprocess, sys, os, time


def read_ppm(path):
    with open(path, 'rb') as f:
        data = f.read()
    parts, i = [], 0
    while len(parts) < 4:
        while data[i:i + 1].isspace():
            i += 1
        if data[i:i + 1] == b'#':
            while data[i:i + 1] != b'\n':
                i += 1
            continue
        j = i
        while not data[j:j + 1].isspace():
            j += 1
        parts.append(data[i:j])
        i = j
    w, h = int(parts[1]), int(parts[2])
    return w, h, data[i + 1:i + 1 + w * h * 3]


def main():
    exe, out = sys.argv[1], sys.argv[2]
    backend = sys.argv[3] if len(sys.argv) > 3 else ''
    os.makedirs(out, exist_ok=True)
    env = dict(os.environ, NOVAUI_GALLERY_OUT=out)
    p = subprocess.Popen([exe, backend], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, text=True, encoding="utf-8")
    failures, phases = 0, 0
    t0 = time.time()
    for line in p.stdout:
        line = line.rstrip()
        print('app:', line, flush=True)
        if not line.startswith('READY'):
            continue
        _, n, win, size = line.split()
        time.sleep(0.3)  # let the compositor show the presented frame
        shot = os.path.join(out, f'screen-{n}.ppm')
        subprocess.run(['import', '-window', hex(int(win)), '-depth', '8', shot], check=True)
        sw, sh, s = read_ppm(shot)
        rw, rh, r = read_ppm(os.path.join(out, f'ref-{n}.ppm'))
        if (sw, sh) != (rw, rh):
            print(f'phase {n}: size mismatch screen {sw}x{sh} ref {rw}x{rh}')
            failures += 1
            continue
        worst, over2, over3 = 0, 0, 0
        for k in range(0, len(s), 3):
            d = max(abs(s[k] - r[k]), abs(s[k + 1] - r[k + 1]), abs(s[k + 2] - r[k + 2]))
            if d > worst:
                worst = d
            if d > 2:
                over2 += 1
            if d > 3:
                over3 += 1
        ok = over3 == 0 and over2 <= 16
        phases += 1
        failures += 0 if ok else 1
        print(f'phase {n}: {sw}x{sh} max diff {worst}, >2: {over2} px, >3: {over3} px -> {"OK" if ok else "FAIL"}', flush=True)
        if time.time() - t0 > 60:
            break
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        failures += 1
        print('app did not close')
    print(f'{phases} phases, {failures} failures')
    sys.exit(0 if failures == 0 and phases >= 2 else 1)


main()

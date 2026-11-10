from pathlib import Path
import argparse, ctypes as c, ctypes.wintypes as w, json, os, sqlite3, subprocess, time
from PIL import Image, ImageChops, ImageGrab

# Optional Windows-only regression check. Pillow is a development tool; the
# player never depends on Python. Every launched app uses an isolated fixture.
#
# Drives the native window through minimize, restore, maximize and the tray
# path and captures the screen WITHOUT moving the pointer: the frame Windows
# shows right after the transition must already contain the interface.
if os.name != 'nt':
    raise SystemExit('Native window-state verification requires Windows.')
parser = argparse.ArgumentParser(description='Verify the interface is painted after minimize, restore and maximize (solid and acrylic, both renderers).')
parser.add_argument('executable', type=Path)
parser.add_argument('--settle', type=float, default=1.2, help='seconds to wait after each transition')
args = parser.parse_args()
out = Path.cwd() / '.work/window-restore'
out.mkdir(parents=True, exist_ok=True)
exe = args.executable.resolve()
u = c.WinDLL('user32', use_last_error=True)
u.SetThreadDpiAwarenessContext.argtypes = [c.c_void_p]
u.SetThreadDpiAwarenessContext.restype = c.c_void_p
u.SetThreadDpiAwarenessContext(c.c_void_p(-4))
u.GetClientRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
u.ClientToScreen.argtypes = [w.HWND, c.POINTER(w.POINT)]
u.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
u.IsWindowVisible.argtypes = [w.HWND]
u.IsIconic.argtypes = [w.HWND]
u.IsZoomed.argtypes = [w.HWND]
u.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
u.ShowWindow.argtypes = [w.HWND, c.c_int]
u.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
u.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.SendMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.SendMessageW.restype = w.LPARAM
u.SetForegroundWindow.argtypes = [w.HWND]
callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE = 0, 3, 6, 9
WM_CLOSE, WM_SYSCOMMAND, WM_LBUTTONDBLCLK = 0x10, 0x112, 0x203
SC_MINIMIZE, SC_MAXIMIZE, SC_RESTORE = 0xF020, 0xF030, 0xF120
TRAY_MESSAGE = 0x8000 + 41  # src/native.rs

def find_window(pid):
    # femtovg also spawns a visible untitled 16x16 helper window; match the real
    # app window by title so helpers never absorb ShowWindow/tray messages.
    found = []
    @callback
    def enum(h, _):
        owner = w.DWORD()
        u.GetWindowThreadProcessId(h, c.byref(owner))
        if owner.value == pid and u.IsWindowVisible(h):
            title = c.create_unicode_buffer(128)
            u.GetWindowTextW(h, title, 128)
            if title.value == 'Monolith': found.append(h)
        return True
    u.EnumWindows(enum, 0)
    return found[0] if found else None

def client(h):
    r = w.RECT(); p = w.POINT()
    assert u.GetClientRect(h, c.byref(r)) and u.ClientToScreen(h, c.byref(p))
    return p.x, p.y, p.x + r.right, p.y + r.bottom

def edges(im):
    # Hard horizontal edges: glyphs, icons and control borders. A bare acrylic
    # or solid backdrop has almost none, whatever is behind the window.
    grey = im.convert('L')
    diff = ImageChops.difference(grey, grey.transform(grey.size, 0, (1, 0, 1, 0, 1, 0)))
    return sum(diff.histogram()[48:])

def changed(a, b):
    assert a.size == b.size, f'frame sizes differ: {a.size} vs {b.size}'
    return sum(ImageChops.difference(a, b).convert('L').histogram()[25:]) / (a.width * a.height)

results = []
failures = []
for glass, low_power in ((0, 1), (1, 1), (0, 0), (1, 0)):
    tag = f"{'acrylic' if glass else 'solid'}-{'software' if low_power else 'femtovg'}"
    data = out / f'data-{tag}'; music = data / 'music'; music.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(data / 'library.sqlite3') as db:
        db.execute('CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL)')
        db.executemany('INSERT OR REPLACE INTO settings VALUES(?,?)', [('music_folder', str(music)), ('download_folder', str(music)), ('guide_seen', '1'), ('theme_hint_seen', '1'), ('glass', str(glass)), ('low_power', str(low_power)), ('rail_collapsed', '0'), ('theme', '2'), ('current', '-1'), ('position', '0'), ('volume', '0')])
    env = os.environ.copy(); env['MONOLITH_DATA_DIR'] = str(data); env['SLINT_SCALE_FACTOR'] = '1'
    log = (out / f'{tag}.err').open('w', encoding='utf-8')
    app = subprocess.Popen([str(exe)], env=env, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
    h = None
    try:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            h = find_window(app.pid)
            if h: break
            assert app.poll() is None, 'player exited during startup'; time.sleep(.2)
        assert h, 'player window not found'
        time.sleep(3)
        assert u.SetWindowPos(h, w.HWND(-1), 20, 20, min(1100, u.GetSystemMetrics(0) - 40), min(720, u.GetSystemMetrics(1) - 80), 0x0040)
        u.SetForegroundWindow(h); time.sleep(1.5)

        def shot(name):
            # A restore in flight can still report the hidden/minimized
            # (-32000) placement, which hands ImageGrab an off-screen bbox and
            # a degenerate image that crashes on save. Wait for the window to
            # be visible at a real on-screen rectangle first.
            box = None
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if u.IsWindowVisible(h):
                    x1, y1, x2, y2 = client(h)
                    if x2 > x1 and y2 > y1 and x1 >= 0 and y1 >= 0:
                        box = (x1, y1, x2, y2)
                        break
                time.sleep(.15)
            assert box, f'{tag} {name}: window never became visible on-screen'
            raw = ImageGrab.grab(bbox=box, all_screens=True)
            im = Image.frombytes('RGB', raw.size, raw.convert('RGB').tobytes())
            im.save(out / f'{tag}-{name}.png')
            return im

        def step(name, action, reference=None):
            # `reference` is the frame this state showed before the transition.
            action(); time.sleep(args.settle)
            assert app.poll() is None, f'{name}: player exited'
            frame = shot(name)
            row = {'config': tag, 'step': name, 'size': frame.size, 'edges': edges(frame)}
            if reference is not None and reference.size == frame.size:
                row['changed'] = round(changed(reference, frame), 4)
            # Edge count is fixed by the controls, not the window area: a UI at
            # 100% scale draws the same glyphs at any size, so the floor must
            # not grow with the frame. A bare backdrop only has gradients.
            row['ok'] = row['edges'] >= floor and row.get('changed', 0) < 0.02
            results.append(row); print(row, flush=True)
            if not row['ok']: failures.append(f"{tag} {name}: edges {row['edges']}, changed {row.get('changed')}")
            return frame

        normal = shot('0-normal')
        floor = edges(normal) * 0.5
        assert floor > 500, f'{tag}: baseline frame has no interface ({edges(normal)} edges)'
        print({'config': tag, 'step': '0-normal', 'size': normal.size, 'edges': edges(normal)}, flush=True)

        show = lambda mode: lambda: u.ShowWindow(h, mode)
        system = lambda command: lambda: u.SendMessageW(h, WM_SYSCOMMAND, command, 0)
        def minimized(action):
            def run():
                action(); time.sleep(args.settle)
                assert u.IsIconic(h), 'window did not minimize'
            return run

        minimized(show(SW_MINIMIZE))()
        step('1-restore', show(SW_RESTORE), normal)
        minimized(system(SC_MINIMIZE))()
        step('2-taskbar-restore', system(SC_RESTORE), normal)
        maximized = step('3-maximize', show(SW_MAXIMIZE))
        assert u.IsZoomed(h), 'window did not maximize'
        minimized(system(SC_MINIMIZE))()
        step('4-restore-to-maximized', system(SC_RESTORE), maximized)
        step('5-restore-from-maximized', system(SC_RESTORE), normal)
        step('6-maximize-command', system(SC_MAXIMIZE), maximized)
        step('7-unmaximize', show(SW_RESTORE), normal)
        # Tray: the player minimizes then hides its window; the tray icon's
        # double-click message brings it back.
        def to_tray():
            u.ShowWindow(h, SW_MINIMIZE); time.sleep(.4); u.ShowWindow(h, SW_HIDE)
            time.sleep(args.settle)
            assert not u.IsWindowVisible(h), 'window did not hide'
        to_tray()
        step('8-tray-restore', lambda: u.SendMessageW(h, TRAY_MESSAGE, 0, WM_LBUTTONDBLCLK), normal)
    finally:
        if h: u.PostMessageW(h, WM_CLOSE, 0, 0)
        try: app.wait(timeout=15)
        except subprocess.TimeoutExpired: app.terminate(); app.wait(timeout=5)
        log.close()
(out / 'result.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
if failures:
    raise SystemExit('WINDOW RESTORE CHECK FAILED\n' + '\n'.join(failures))
print(f'WINDOW RESTORE CHECK COMPLETE ({len(results)} transitions painted)', flush=True)

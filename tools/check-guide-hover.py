from pathlib import Path
import ctypes as c, ctypes.wintypes as w, os, sqlite3, subprocess, time, wave, json
from PIL import Image, ImageGrab, ImageChops

# Optional Windows-only regression check. Pillow is a development tool; the
# player never depends on Python. Every launched app uses an isolated fixture.
import argparse
if os.name != 'nt':
    raise SystemExit('Native hover verification requires Windows.')
parser = argparse.ArgumentParser(description='Verify native guide partial redraw during fixture playback (solid and acrylic).')
parser.add_argument('executable', type=Path)
args = parser.parse_args()
root = Path.cwd()
out = root / '.work/guide-hover'
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
u.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
u.PostMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.SetForegroundWindow.argtypes = [w.HWND]
u.SendMessageW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
u.SendMessageW.restype = w.LPARAM
callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)

def find_window(pid):
    found = []
    @callback
    def enum(h, _):
        owner = w.DWORD()
        u.GetWindowThreadProcessId(h, c.byref(owner))
        if owner.value == pid and u.IsWindowVisible(h): found.append(h)
        return True
    u.EnumWindows(enum, 0)
    return found[0] if found else None

def client(h):
    r = w.RECT(); p = w.POINT()
    assert u.GetClientRect(h, c.byref(r)) and u.ClientToScreen(h, c.byref(p))
    return p.x, p.y, p.x + r.right, p.y + r.bottom

def move(h, x, y):
    left, top, _, _ = client(h)
    u.SetCursorPos(round(left+x), round(top+y))
    u.SendMessageW(h, 0x200, 0, (round(y)<<16)|round(x))

def click(h, x, y):
    move(h, x, y)
    pos = (round(y)<<16)|round(x)
    u.SendMessageW(h, 0x201, 1, pos)
    u.SendMessageW(h, 0x202, 0, pos)
    time.sleep(.65)

def shot(h, name):
    im = ImageGrab.grab(bbox=client(h)).convert('RGB')
    im.save(out/f'{name}.png')
    return im

def card(im):
    # Largest horizontal run of the coach surface at its interior probe line.
    target = (32,32,37)
    rows=[]
    for y in range(10, im.height-90):
        xs=[x for x in range(80,im.width-10) if im.getpixel((x,y)) == target]
        if len(xs)>280: rows.append(y)
    assert rows, 'Guide card not found'
    top,bottom=min(rows),max(rows)+1
    # Interior row away from letters and rounded edges.
    y=top+4
    xs=[x for x in range(80,im.width-10) if im.getpixel((x,y)) == target]
    return min(xs),top,max(xs)+1,bottom

results=[]
for glass in (0,1):
    data=out/f'data-{glass}'; music=data/'music'; music.mkdir(parents=True,exist_ok=True)
    with sqlite3.connect(data/'library.sqlite3') as db:
        db.execute('CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL)')
        db.executemany('INSERT OR REPLACE INTO settings VALUES(?,?)', [('music_folder',str(music)),('download_folder',str(music)),('guide_seen','1'),('theme_hint_seen','1'),('glass',str(glass)),('rail_collapsed','0'),('theme','2'),('current','-1'),('position','0'),('volume','0')])
    audio=music/'Hover fixture.wav'
    with wave.open(str(audio),'wb') as f:
        f.setnchannels(1);f.setsampwidth(2);f.setframerate(8000);f.writeframes(b'\0\0'*8000*180)
    Image.new('RGB',(200,160),(220,80,60)).save(audio.with_suffix('.webp'))
    env=os.environ.copy();env['MONOLITH_DATA_DIR']=str(data);env['SLINT_SCALE_FACTOR']='1'
    log=(out/f'{glass}.err').open('w',encoding='utf-8')
    app=subprocess.Popen([str(exe)],env=env,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        deadline=time.monotonic()+20; h=None
        while time.monotonic()<deadline:
            h=find_window(app.pid)
            if h: break
            assert app.poll() is None;time.sleep(.2)
        assert h
        time.sleep(3)
        assert u.SetWindowPos(h,w.HWND(-1),20,20,min(1280,u.GetSystemMetrics(0)-40),min(850,u.GetSystemMetrics(1)-40),0x0040)
        u.SetForegroundWindow(h);time.sleep(1)
        print('geometry',client(h),'screen',u.GetSystemMetrics(0),u.GetSystemMetrics(1),flush=True)
        width=client(h)[2]-client(h)[0];height=client(h)[3]-client(h)[1]
        click(h,420,234)  # Real playback keeps partial redraw active beneath the tour.
        time.sleep(.5)
        click(h,30,height-88-32)
        move(h,10,10);time.sleep(.7)
        before=shot(h,f'{glass}-guide-before')
        bounds=card(before);left,top,right,bottom=bounds
        print('card',glass,bounds,'client',width,height,flush=True)
        move(h,left+125,bottom-38)
        time.sleep(.3)
        hover=shot(h,f'{glass}-close-hover')
        # Hover is allowed to change the card, never the backdrop outside it.
        diff=ImageChops.difference(before,hover)
        diff.paste((0,0,0),(max(0,left-15),max(0,top-15),min(width,right+15),min(height,bottom+15)))
        diff.paste((0,0,0),(0,height-88,width,height)) # Real elapsed time/seek can advance.
        bad=sum(max(px)>1 for px in diff.get_flattened_data())
        black=sum(px == (0,0,0) for px in hover.get_flattened_data())
        results.append({'glass':glass,'outside_card_changed_pixels':bad,'black_pixels':black,'card':bounds})
        print(results[-1],flush=True)
        assert bad<30, f'Hover corrupts {bad} pixels outside the guide card'
        assert black<1500, f'Initial guide: {black} black artifact pixels'
        assert hover.getpixel((right-100,bottom-38)) == (32,32,37), 'Hover corrupts the card action spacer'
        # Exercise successive moving spotlights with real animation enabled.
        for step in range(1,15):
            move(h,10,10)
            click(h,right-43,bottom-38)
            time.sleep(.8)
            plain=shot(h,f'{glass}-step-{step}')
            left,top,right,bottom=card(plain)
            move(h,left+125,bottom-38);time.sleep(.25)
            hovered=shot(h,f'{glass}-step-{step}-hover')
            diff=ImageChops.difference(plain,hovered)
            diff.paste((0,0,0),(max(0,left-15),max(0,top-15),min(width,right+15),min(height,bottom+15)))
            diff.paste((0,0,0),(0,height-88,width,height))
            bad=sum(max(px)>1 for px in diff.get_flattened_data())
            black=sum(px == (0,0,0) for px in hovered.get_flattened_data())
            results.append({'glass':glass,'step':step,'outside_card_changed_pixels':bad,'black_pixels':black,'card':[left,top,right,bottom]})
            assert bad<30, f'Step {step}: hover corrupts {bad} pixels'
            assert black<1500, f'Step {step}: {black} black artifact pixels'
            assert hovered.getpixel((right-100,bottom-38)) == (32,32,37), f'Step {step}: card action spacer is corrupted'
    finally:
        if h: u.PostMessageW(h,0x10,0,0)
        try:app.wait(timeout=15)
        except subprocess.TimeoutExpired:app.terminate();app.wait(timeout=5)
        log.close()
(out/'result.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
print('NATIVE HOVER CHECK COMPLETE',flush=True)

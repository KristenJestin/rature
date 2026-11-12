"""Types keys into the X display named by DISPLAY, through XTEST: `xkeys.py j j shift+g ctrl+d`.
`click:X,Y` clicks there, `ctrl+click:X,Y` with Ctrl held; `wheel:up:X,Y` (or `down`) turns the wheel
a notch there, `ctrl+wheel:up:X,Y` with Ctrl held; `drag:X,Y,X2,Y2` drags from one point to the
other, `space+drag:...` with Space held.
Used by tools/capture.sh on its nested headless display, never the user's."""
import ctypes
import sys
import time

x11 = ctypes.cdll.LoadLibrary("libX11.so.6")
xtst = ctypes.cdll.LoadLibrary("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]

display = x11.XOpenDisplay(None)
if not display:
    sys.exit("xkeys: no display")
NAMES = {"shift": "Shift_L", "ctrl": "Control_L", "alt": "Alt_L", "space": "space"}


def code(name):
    return x11.XKeysymToKeycode(display, x11.XStringToKeysym(NAMES.get(name, name).encode()))


def move(x, y):
    xtst.XTestFakeMotionEvent(display, -1, int(x), int(y), 0)
    x11.XFlush(display)


def button(number, down):
    xtst.XTestFakeButtonEvent(display, number, down, 0)
    x11.XFlush(display)


# The nested X server drops the first key it gets: a Shift tap takes that place.
for chord in ["shift"] + sys.argv[1:]:
    parts = chord.split("+")
    pointer = parts[-1].startswith(("click:", "wheel:", "drag:")) and parts.pop()
    keys = [code(k) for k in parts]
    for k in keys:
        xtst.XTestFakeKeyEvent(display, k, True, 0)
    x11.XFlush(display)
    if pointer:
        kind, at = pointer.split(":", 1)
        number = 1
        if kind == "wheel":
            way, at = at.split(":", 1)
            # X has the wheel as buttons 4 (up) and 5 (down).
            number = 4 if way == "up" else 5
        points = [int(v) for v in at.split(",")]
        move(*points[:2])
        time.sleep(0.1)
        button(number, True)
        if kind == "drag":
            (x, y), (x2, y2) = points[:2], points[2:]
            for step in range(1, 11):
                move(x + (x2 - x) * step / 10, y + (y2 - y) * step / 10)
                time.sleep(0.03)
        button(number, False)
    for k in reversed(keys):
        xtst.XTestFakeKeyEvent(display, k, False, 0)
    x11.XFlush(display)
    time.sleep(0.15)

"""Types keys into the X display named by DISPLAY, through XTEST: `xkeys.py j j shift+g ctrl+d`.
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
x11.XFlush.argtypes = [ctypes.c_void_p]

display = x11.XOpenDisplay(None)
if not display:
    sys.exit("xkeys: no display")
NAMES = {"shift": "Shift_L", "ctrl": "Control_L", "space": "space"}


def code(name):
    return x11.XKeysymToKeycode(display, x11.XStringToKeysym(NAMES.get(name, name).encode()))


# The nested X server drops the first key it gets: a Shift tap takes that place.
for chord in ["shift"] + sys.argv[1:]:
    keys = [code(k) for k in chord.split("+")]
    for k in keys:
        xtst.XTestFakeKeyEvent(display, k, True, 0)
    for k in reversed(keys):
        xtst.XTestFakeKeyEvent(display, k, False, 0)
    x11.XFlush(display)
    time.sleep(0.15)

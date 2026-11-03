#!/usr/bin/env python3
"""Drive the X server's pointer with XTest, for wheel ground truth.

    python3 xtest.py wheel --x 640 --y 400 --down 3 --up 2 --gap-ms 300

Moves the pointer to (x, y) on $DISPLAY and clicks wheel buttons: 5 is one
notch down, 4 one notch up. Only for a headed browser under xvfb-run on
this machine (loopback capture, tools/os_truth/capture.js). Uses ctypes and
the system libX11/libXtst; no other dependencies.
"""

from __future__ import annotations

import argparse
import ctypes
import sys
import time


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["wheel"])
    ap.add_argument("--x", type=int, required=True)
    ap.add_argument("--y", type=int, required=True)
    ap.add_argument("--down", type=int, default=3)
    ap.add_argument("--up", type=int, default=0)
    ap.add_argument("--gap-ms", type=int, default=300)
    a = ap.parse_args(argv)

    x11 = ctypes.CDLL("libX11.so.6")
    xtst = ctypes.CDLL("libXtst.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XFlush.argtypes = [ctypes.c_void_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
    xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

    dpy = x11.XOpenDisplay(None)
    if not dpy:
        print("cannot open $DISPLAY", file=sys.stderr)
        return 2
    try:
        xtst.XTestFakeMotionEvent(dpy, -1, a.x, a.y, 0)
        x11.XFlush(dpy)
        time.sleep(0.3)
        for button, n in ((5, a.down), (4, a.up)):
            for _ in range(n):
                xtst.XTestFakeButtonEvent(dpy, button, 1, 0)
                xtst.XTestFakeButtonEvent(dpy, button, 0, 0)
                x11.XFlush(dpy)
                time.sleep(a.gap_ms / 1000)
    finally:
        x11.XCloseDisplay(dpy)
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Close the app from its window and require a clean exit.

Usage: python scripts/test-clean-exit.py target/package/windows/flectar-mail.exe

Starts the app with the current user's profile (a fresh one on CI), waits
until its local data is ready, posts WM_CLOSE to its window the way the title
bar's close button does, and fails unless the process then exits with code 0.
Only a close from the window runs the full shutdown: ending the event loop
while the window is still shown keeps the window's state alive. A profile
that this run created is removed afterwards, so later checks, such as the
Store package test, start from a clean profile. Windows only.
"""
import ctypes
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

METRIC_PREFIX = "FLECTAR_STARTUP_METRIC "
READY_EVENT = "core_ready_frame"
READY_SECONDS = 180
SETTLE_SECONDS = 2
EXIT_SECONDS = 30
WM_CLOSE = 0x0010
GW_OWNER = 4
GWL_EXSTYLE = -20
WS_EX_TOOLWINDOW = 0x00000080


def read_lines(stream, lines):
    for line in iter(stream.readline, b""):
        lines.put(line.decode("utf-8", errors="replace").rstrip())
    lines.put(None)


def wait_until_ready(process, lines, output):
    deadline = time.monotonic() + READY_SECONDS
    while time.monotonic() < deadline:
        try:
            line = lines.get(timeout=0.5)
        except queue.Empty:
            continue
        if line is None:
            process.wait()
            fail(f"the app exited with {exit_code(process.returncode)} before it was ready", output)
        output.append(line)
        if line.startswith(METRIC_PREFIX):
            try:
                event = json.loads(line[len(METRIC_PREFIX):]).get("event")
            except ValueError:
                continue
            if event == READY_EVENT:
                return
    fail(f"the app was not ready within {READY_SECONDS} s", output)


def drain(lines, output, seconds):
    deadline = time.monotonic() + seconds
    while (remaining := deadline - time.monotonic()) > 0:
        try:
            line = lines.get(timeout=remaining)
        except queue.Empty:
            return
        if line is None:
            return
        output.append(line)


def app_windows(pid):
    """The process's windows that the taskbar lists, which its close button
    and Task Manager's End task close.

    Helper windows are left alone. winit keeps a visible tool window for its
    own messages, and closing it would destroy it along with the event loop's
    wakeups, so the app could never quit.
    """
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    enum_proc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = [enum_proc, wintypes.LPARAM]
    user32.EnumWindows.restype = wintypes.BOOL
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.GetWindowThreadProcessId.restype = wintypes.DWORD
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.IsWindowVisible.restype = wintypes.BOOL
    user32.GetWindow.argtypes = [wintypes.HWND, wintypes.UINT]
    user32.GetWindow.restype = wintypes.HWND
    user32.GetWindowLongW.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.GetWindowLongW.restype = wintypes.LONG

    windows = []

    def collect(hwnd, _):
        process = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(process))
        if (
            process.value == pid
            and user32.IsWindowVisible(hwnd)
            and not user32.GetWindow(hwnd, GW_OWNER)
            and not user32.GetWindowLongW(hwnd, GWL_EXSTYLE) & WS_EX_TOOLWINDOW
        ):
            windows.append(hwnd)
        return True

    if not user32.EnumWindows(enum_proc(collect), 0):
        raise ctypes.WinError(ctypes.get_last_error())
    return windows


def close_windows(pid):
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    user32.PostMessageW.restype = wintypes.BOOL
    windows = app_windows(pid)
    for hwnd in windows:
        if not user32.PostMessageW(hwnd, WM_CLOSE, 0, 0):
            raise ctypes.WinError(ctypes.get_last_error())
    return len(windows)


def profile_dirs():
    """The desktop profile: data under %APPDATA%, caches under %LOCALAPPDATA%."""
    return [Path(os.environ[name]) / "flectar-mail" for name in ("APPDATA", "LOCALAPPDATA")]


def remove(paths):
    for path in paths:
        if path.exists():
            try:
                shutil.rmtree(path)
            except OSError as error:
                print(f"warning: could not remove {path}: {error}", file=sys.stderr)


def exit_code(code):
    return f"{code & 0xFFFFFFFF:#010x}" if code < 0 or code > 255 else str(code)


def fail(message, output):
    print(f"FAIL: {message}", file=sys.stderr)
    print("\n".join(output[-40:]), file=sys.stderr)
    sys.exit(1)


def main(binary):
    if os.name != "nt":
        sys.exit("test-clean-exit.py closes a native Windows window; run it on Windows")
    environment = dict(os.environ, FLECTAR_STARTUP_METRICS="1")
    # Only a profile this run creates is removed, never one that was there.
    created = [path for path in profile_dirs() if not path.exists()]
    process = subprocess.Popen(
        [binary],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        env=environment,
    )
    lines = queue.Queue()
    threading.Thread(target=read_lines, args=(process.stderr, lines), daemon=True).start()
    output = []
    try:
        started = time.monotonic()
        wait_until_ready(process, lines, output)
        ready = time.monotonic() - started
        time.sleep(SETTLE_SECONDS)
        if process.poll() is not None:
            drain(lines, output, 5)
            fail(f"the app exited with {exit_code(process.returncode)} before it was closed", output)
        if close_windows(process.pid) == 0:
            fail("the app showed no window to close", output)
        closed = time.monotonic()
        try:
            process.wait(timeout=EXIT_SECONDS)
        except subprocess.TimeoutExpired:
            drain(lines, output, 1)
            window = "its window is still open" if app_windows(process.pid) else "its window closed"
            fail(f"the app did not exit within {EXIT_SECONDS} s of WM_CLOSE ({window})", output)
        exited = time.monotonic() - closed
        drain(lines, output, 5)
        if process.returncode != 0:
            fail(f"the app exited with {exit_code(process.returncode)} after WM_CLOSE", output)
        if any("' panicked at " in line for line in output):
            fail("the app panicked while closing", output)
        print(f"PASS: ready after {ready:.1f} s, exited cleanly {exited:.1f} s after WM_CLOSE")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        remove(created)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])

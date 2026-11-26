#!/usr/bin/env python3
"""Windows real-machine E2E checks for draconium.

Covers custom-chrome transparent screenshots, starter screenshot, DevTools
Firefox RDP handshake, tray/single-instance WM_COPYDATA forwarding, and live
HTTP IPC + child window capability attenuation and viewport resizing.
"""

from __future__ import annotations

import argparse
import ctypes
import http.server
import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time
import zlib
from ctypes import wintypes

PARENT_HTML = r"""<!doctype html>
<html><head><title>VerifyLive</title></head><body><script>
(async () => {
  const r = {};
  try {
    const enc = window.Draconium.safeStorage.encrypt("dpapi-live-日本語");
    const dec = enc.ok ? window.Draconium.safeStorage.decrypt(enc.data) : null;
    const encA = await window.Draconium.safeStorage.encryptAsync("dpapi-async");
    const decA = encA.ok ? await window.Draconium.safeStorage.decryptAsync(encA.data) : null;
    r.safeStorage = { encOk: enc.ok, decVal: dec && dec.data, asyncVal: decA && decA.data };

    const sw = window.Draconium.clipboard.writeText("dn-live-sync-日本語");
    const sr = window.Draconium.clipboard.readText();
    const cw = await window.Draconium.clipboard.writeTextAsync("dn-live-clip-日本語");
    const cr = await window.Draconium.clipboard.readTextAsync();
    const hw = window.Draconium.clipboard.writeHtml("<b>Hi</b>");
    const hr = window.Draconium.clipboard.readHtml();
    const px = new Uint8Array([255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,255,255]);
    let b64 = "";
    for (let i = 0; i < px.length; i++) b64 += String.fromCharCode(px[i]);
    const iw = window.Draconium.clipboard.writeImage({ width: 2, height: 2, rgbaBase64: btoa(b64) });
    const ir = window.Draconium.clipboard.readImage();
    const cc = window.Draconium.clipboard.clear();
    r.clipboard = {
      sw: sw.ok, sr: sr.data, cw: cw.ok, cr: cr.data,
      hw: hw.ok, hr: hr.data,
      iw: iw.ok, irOk: ir.ok, irW: ir.data && ir.data.width, irH: ir.data && ir.data.height,
      cc: cc.ok
    };

    try { localStorage.setItem("dn_session_probe", "1"); } catch (e) { r.lsErr = String(e); }
    const sd1 = await window.Draconium.session.listSiteDataAsync();
    const ccCache = await window.Draconium.session.clearCacheAsync();
    const ccCookies = await window.Draconium.session.clearCookiesAsync({ sessionOnly: false });
    const ccSite = await window.Draconium.session.clearSiteDataAsync();
    r.session = {
      sd1Ok: sd1.ok,
      sd1Count: Array.isArray(sd1.data) ? sd1.data.length : -1,
      clearCache: ccCache.ok,
      clearCookies: ccCookies.ok,
      clearSiteData: ccSite.ok
    };

    const op1 = window.Draconium.window.setOpacity(0.85);
    const op2 = window.Draconium.window.setOpacity(1.0);
    const ct = window.Draconium.window.setClickThrough(false);
    r.opacity = { op1: op1.ok, op2: op2.ok, ct: ct.ok };

    r.printUsesExportPdf = String(window.print).includes("exportPdf");
    r.wasmValid = WebAssembly.validate(new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]));

    const childUrl = location.origin + "/child.html";
    const oc = await window.Draconium.window.openChildAsync({ url: childUrl, title: "DraconiumChildVerify-" + location.port, width: 320, height: 240 });
    r.openChild = oc;

    const badChild = window.Draconium.window.openChild({ url: "https://example.com" });
    r.badChildRejected = !badChild.ok;

    const n = window.Draconium.notify({ title: "DN", body: "VerifyNotice" });
    r.notify = n;
  } catch (err) {
    r.fatal = String(err && (err.stack || err));
  }
  await fetch("/report/parent", { method: "POST", body: JSON.stringify(r) });
})();
</script></body></html>
"""

CHILD_HTML = r"""<!doctype html>
<html><body><script>
(async () => {
  const c = {};
  try {
    c.hasBridge = Boolean(window.Draconium && Object.isFrozen(window.Draconium));
    c.deniedSafe = !window.Draconium.safeStorage.encrypt("nope").ok;
    c.deniedApp = !window.Draconium.invoke("list_notes", "{}").ok;
    c.deniedClip = !window.Draconium.clipboard.readText().ok;
    c.deniedSite = !window.Draconium.session.listSiteData().ok;
    c.deniedChild = !window.Draconium.window.openChild({ url: "about:blank" }).ok;
    c.themeOk = window.Draconium.window.getTheme().ok;
    c.viewport = { width: window.innerWidth, scale: window.devicePixelRatio };
    async function reportResize() {
      if (window.innerWidth < c.viewport.width + 50) return;
      window.removeEventListener("resize", reportResize);
      await fetch("/report/child-resize", {
        method: "POST",
        body: JSON.stringify({ width: window.innerWidth }),
      });
    }
    window.addEventListener("resize", reportResize);
  } catch (err) {
    c.fatal = String(err && (err.stack || err));
  }
  await fetch("/report/child", { method: "POST", body: JSON.stringify(c) });
})();
</script></body></html>
"""


def parse_png(path: str) -> tuple[int, int, int, int, list[bytes]]:
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"Not a PNG file: {path}")
    pos = 8
    w = h = ct = 0
    idat = bytearray()
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        ctype = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + length]
        if ctype == b"IHDR":
            w, h, bd, ct = struct.unpack(">IIBB", chunk[:10])
            if bd != 8:
                raise ValueError(f"Unsupported PNG bit depth {bd}: {path}")
        elif ctype == b"IDAT":
            idat.extend(chunk)
        elif ctype == b"IEND":
            break
        pos += 12 + length
    if ct not in (2, 6):
        raise ValueError(f"Unsupported PNG color type {ct}: {path}")
    bpp = 4 if ct == 6 else 3
    stride = w * bpp
    raw = zlib.decompress(bytes(idat))
    rows: list[bytes] = []
    prev = bytes(stride)
    i = 0
    for _ in range(h):
        filt = raw[i]
        i += 1
        row = bytearray(raw[i : i + stride])
        i += stride
        if filt == 1:
            for x in range(bpp, stride):
                row[x] = (row[x] + row[x - bpp]) & 0xFF
        elif filt == 2:
            for x in range(stride):
                row[x] = (row[x] + prev[x]) & 0xFF
        elif filt == 3:
            for x in range(stride):
                a = row[x - bpp] if x >= bpp else 0
                row[x] = (row[x] + ((a + prev[x]) >> 1)) & 0xFF
        elif filt == 4:
            for x in range(stride):
                a = row[x - bpp] if x >= bpp else 0
                b = prev[x]
                c = prev[x - bpp] if x >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[x] = (row[x] + pr) & 0xFF
        elif filt != 0:
            raise ValueError(f"Unsupported PNG filter {filt}: {path}")
        row_bytes = bytes(row)
        rows.append(row_bytes)
        prev = row_bytes
    return w, h, ct, bpp, rows


def inspect_png(path: str) -> dict:
    w, h, ct, bpp, rows = parse_png(path)
    min_alpha = 255
    max_alpha = 0
    for r in rows:
        if bpp == 4:
            for x in range(w):
                a = r[x * bpp + 3]
                if a < min_alpha:
                    min_alpha = a
                if a > max_alpha:
                    max_alpha = a
    corner_br = tuple(rows[h - 1][(w - 1) * bpp : (w - 1) * bpp + bpp])
    return {
        "width": w,
        "height": h,
        "color_type": ct,
        "min_alpha": min_alpha if bpp == 4 else 255,
        "max_alpha": max_alpha if bpp == 4 else 255,
        "corner_br": corner_br,
    }


def run_checked(cmd: list[str], *, cwd: str | None = None, timeout: float = 30.0) -> str:
    res = subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )
    combined = (res.stdout or "") + (res.stderr or "")
    if res.returncode != 0:
        raise RuntimeError(f"Command failed (exit={res.returncode}): {cmd}\n{combined}")
    return combined


def test_screenshots(exe: str, work_dir: str) -> None:
    ws_dir = os.path.join(work_dir, "workspace-shot")
    os.makedirs(ws_dir, exist_ok=True)
    ws_png = os.path.join(work_dir, "workspace.png")
    ws_out = run_checked(
        [exe, "--workspace", "--dn-assets", "--workspace-dir", ws_dir, "--screenshot", ws_png]
    )
    assert "non_blank=true" in ws_out, f"workspace screenshot unexpected output: {ws_out}"
    ws_info = inspect_png(ws_png)
    assert ws_info["width"] == 1240 and ws_info["height"] == 800, (
        f"Unexpected workspace dimensions: {ws_info}"
    )
    assert ws_info["color_type"] == 6 and ws_info["min_alpha"] == 0, (
        f"Expected custom-chrome transparent rounded corners: {ws_info}"
    )
    print(f"[PASS] workspace custom-chrome screenshot: {ws_info['width']}x{ws_info['height']} min_alpha={ws_info['min_alpha']}")

    starter_png = os.path.join(work_dir, "starter.png")
    st_out = run_checked([exe, "--starter", "--dn-assets", "--screenshot", starter_png])
    assert "non_blank=true" in st_out, f"starter screenshot unexpected output: {st_out}"
    st_info = inspect_png(starter_png)
    assert st_info["width"] == 760 and st_info["height"] == 560, (
        f"Unexpected starter dimensions: {st_info}"
    )
    print(f"[PASS] starter screenshot: {st_info['width']}x{st_info['height']}")


def pick_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def recv_rdp_packet(sock: socket.socket) -> bytes:
    buf = bytearray()
    header_end = -1
    while header_end < 0:
        chunk = sock.recv(4096)
        if not chunk:
            break
        buf.extend(chunk)
        header_end = buf.find(b":")
    if header_end <= 0:
        return bytes(buf)
    length = int(bytes(buf[:header_end]).decode("ascii"))
    total_needed = header_end + 1 + length
    while len(buf) < total_needed:
        chunk = sock.recv(total_needed - len(buf))
        if not chunk:
            break
        buf.extend(chunk)
    return bytes(buf)


def answer_devtools_prompt(process_id: int, *, allow: bool) -> None:
    """Answer the consent dialog of the Draconium process launched by this test."""
    user32 = ctypes.windll.user32
    enum_callback = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = (enum_callback, wintypes.LPARAM)
    user32.GetWindowThreadProcessId.argtypes = (
        wintypes.HWND, ctypes.POINTER(wintypes.DWORD)
    )
    user32.GetWindowTextW.argtypes = (wintypes.HWND, wintypes.LPWSTR, ctypes.c_int)
    user32.GetClassNameW.argtypes = user32.GetWindowTextW.argtypes
    user32.GetDlgItem.argtypes = (wintypes.HWND, ctypes.c_int)
    user32.GetDlgItem.restype = wintypes.HWND
    user32.PostMessageW.argtypes = (
        wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM
    )
    window = None

    @enum_callback
    def find_prompt(hwnd, _):
        nonlocal window
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value != process_id:
            return True
        name = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, name, len(name))
        if name.value == "#32770":
            user32.GetWindowTextW(hwnd, name, len(name))
            if name.value == "Firefox DevTools":
                window = hwnd
                return False
        return True

    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        window = None
        user32.EnumWindows(find_prompt, 0)
        if window:
            button_id = 1 if allow else 2  # IDOK / IDCANCEL
            button = user32.GetDlgItem(window, button_id)
            if button and user32.PostMessageW(window, 0x0111, button_id, button):  # WM_COMMAND
                return
        time.sleep(0.1)
    raise AssertionError("Could not answer the Firefox DevTools consent dialog")


def test_devtools_rdp(exe: str) -> None:
    port = pick_free_port()
    addr = f"127.0.0.1:{port}"
    proc = subprocess.Popen(
        [exe, "--starter", "--dn-assets", "--devtools", "--devtools-addr", addr],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and proc.poll() is None:
            try:
                connection = socket.create_connection(("127.0.0.1", port), timeout=1.0)
            except OSError:
                time.sleep(0.2)
            else:
                break
        else:
            raise AssertionError("DevTools RDP did not start on loopback")

        with connection:
            answer_devtools_prompt(proc.pid, allow=False)
            connection.settimeout(3.0)
            try:
                rejected = connection.recv(1)
            except ConnectionResetError:
                rejected = b""
            assert rejected == b"", "DevTools negotiated after the user denied the connection"

        with socket.create_connection(("127.0.0.1", port), timeout=3.0) as sock:
            answer_devtools_prompt(proc.pid, allow=True)
            banner = recv_rdp_packet(sock)
        assert b'"applicationType"' in banner, f"DevTools RDP handshake missing: {banner!r}"
        print(f"[PASS] DevTools RDP denied by Cancel and ready after approval on {addr}")
    finally:
        proc.terminate()
        try:
            proc.communicate(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate()


def wait_for_window(name: str, timeout: float = 12.0) -> int:
    user32 = ctypes.windll.user32
    user32.FindWindowW.argtypes = (wintypes.LPCWSTR, wintypes.LPCWSTR)
    user32.FindWindowW.restype = wintypes.HWND
    deadline = time.time() + timeout
    while time.time() < deadline:
        hwnd = user32.FindWindowW(name, None) or user32.FindWindowW(None, name)
        if hwnd:
            return int(hwnd)
        time.sleep(0.15)
    return 0


def test_tray_single_instance(exe: str, work_dir: str) -> None:
    data_dir = os.path.join(work_dir, "ws-tray")
    os.makedirs(data_dir, exist_ok=True)
    proc = subprocess.Popen(
        [
            exe,
            "--workspace",
            "--dn-assets",
            "--tray",
            "--single-instance",
            "--hotkey",
            "Ctrl+Alt+F12",
            "--workspace-dir",
            data_dir,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        win_name = "Draconium.TrayWindow.LocalDraconiumWorkspace"
        hwnd = wait_for_window(win_name, timeout=12.0)
        assert hwnd != 0, f"Primary OS event window {win_name} did not appear"
        out2 = run_checked(
            [
                exe,
                "--workspace",
                "--dn-assets",
                "--tray",
                "--single-instance",
                "--deep-link",
                "draconium://open/verify-ping",
            ],
            timeout=15.0,
        )
        assert "forwarded=true" in out2, f"Second instance did not forward: {out2}"
        print(f"[PASS] tray/single-instance WM_COPYDATA forwarding: hwnd=0x{hwnd:x}")
    finally:
        proc.terminate()
        try:
            out1, _ = proc.communicate(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            out1, _ = proc.communicate()
        if "Shell_NotifyIconW(NIM_ADD) failed" in (out1 or ""):
            print("[INFO] non-desktop environment detected: tray icon skipped cleanly while OS event window stayed active")


def test_live_http(exe: str, work_dir: str) -> None:
    reports: dict = {}
    done_event = threading.Event()
    resized_event = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: object) -> None:
            pass

        def do_GET(self) -> None:
            if self.path.startswith("/parent.html"):
                body = PARENT_HTML.encode("utf-8")
            elif self.path.startswith("/child.html"):
                body = CHILD_HTML.encode("utf-8")
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:
            ln = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(ln).decode("utf-8")
            if self.path == "/report/parent":
                reports["parent"] = json.loads(raw)
            elif self.path == "/report/child":
                reports["child"] = json.loads(raw)
            elif self.path == "/report/child-resize":
                reports["child-resize"] = json.loads(raw)
                resized_event.set()
            self.send_response(204)
            self.end_headers()
            if "parent" in reports and "child" in reports:
                done_event.set()

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()

    ws_dir = os.path.join(work_dir, "ws-live")
    crash_dir = os.path.join(work_dir, "crashes-live")
    os.makedirs(ws_dir, exist_ok=True)
    url = f"http://127.0.0.1:{port}/parent.html"
    proc = subprocess.Popen(
        [
            exe,
            "--workspace",
            "--csp-wasm",
            "--workspace-dir",
            ws_dir,
            "--dev-url",
            url,
            "--crash-dir",
            crash_dir,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        assert done_event.wait(timeout=20.0), f"Timed out waiting for parent/child reports: {reports}"
        viewport = reports["child"].get("viewport")
        assert viewport and viewport["width"] > 0 and viewport["scale"] > 0, reports["child"]
        hwnd = wait_for_window(f"DraconiumChildVerify-{port}", timeout=2.0)
        assert hwnd, "Child window missing before resize"

        set_window_pos = ctypes.WinDLL("user32", use_last_error=True).SetWindowPos
        set_window_pos.argtypes = (
            wintypes.HWND, wintypes.HWND, ctypes.c_int, ctypes.c_int,
            ctypes.c_int, ctypes.c_int, wintypes.UINT,
        )
        set_window_pos.restype = wintypes.BOOL
        scale = viewport["scale"]
        # SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS
        flags = 0x0002 | 0x0004 | 0x0010 | 0x4000
        if not set_window_pos(hwnd, None, 0, 0, round(520 * scale), round(380 * scale), flags):
            raise ctypes.WinError(ctypes.get_last_error())
        assert resized_event.wait(timeout=5.0), f"Child viewport did not resize: {reports}"
        assert reports["child-resize"]["width"] >= viewport["width"] + 50, reports
        assert proc.poll() is None, "Host exited prematurely after child resize"
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.communicate(timeout=5.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.communicate()
        else:
            proc.communicate()
        httpd.shutdown()

    p = reports["parent"]
    c = reports["child"].copy()
    c.pop("viewport")
    assert p.get("safeStorage") == {
        "encOk": True,
        "decVal": "dpapi-live-日本語",
        "asyncVal": "dpapi-async",
    }, f"safeStorage mismatch: {p}"
    assert p.get("clipboard") == {
        "sw": True,
        "sr": "dn-live-sync-日本語",
        "cw": True,
        "cr": "dn-live-clip-日本語",
        "hw": True,
        "hr": "<b>Hi</b>",
        "iw": True,
        "irOk": True,
        "irW": 2,
        "irH": 2,
        "cc": True,
    }, f"clipboard mismatch: {p}"
    sess = p.get("session", {})
    assert sess.get("sd1Ok") and sess.get("sd1Count", 0) >= 1
    assert sess.get("clearCache") and sess.get("clearCookies") and sess.get("clearSiteData")
    assert p.get("opacity") == {"op1": True, "op2": True, "ct": True}
    assert p.get("printUsesExportPdf") is True
    assert p.get("wasmValid") is True
    assert p.get("openChild") == {"ok": True, "data": "opened"}
    assert p.get("badChildRejected") is True
    assert p.get("notify", {}).get("ok") is True

    assert c == {
        "hasBridge": True,
        "deniedSafe": True,
        "deniedApp": True,
        "deniedClip": True,
        "deniedSite": True,
        "deniedChild": True,
        "themeOk": True,
    }, f"child privilege attenuation mismatch: {c}"
    print("[PASS] live HTTP bridge, DPAPI, clipboard, session, wasm, print->exportPdf, child attenuation and resize")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", required=True)
    parser.add_argument("--work-dir", required=True)
    args = parser.parse_args()

    os.makedirs(args.work_dir, exist_ok=True)
    test_screenshots(args.exe, args.work_dir)
    test_devtools_rdp(args.exe)
    test_tray_single_instance(args.exe, args.work_dir)
    test_live_http(args.exe, args.work_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main())

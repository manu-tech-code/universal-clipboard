#!/usr/bin/env python3
"""Throwaway spike: sync the plain-text clipboard between machines on one LAN.

Run the same file on macOS, Windows and Linux (Python 3.8+, no packages needed).
No encryption, no pairing: a shared token only. Never leave it running on a network you do not trust.

    python3 ucspike.py                  # macOS / Linux
    py ucspike.py                       # Windows
    python3 ucspike.py --peer 192.168.1.20   # also try a direct connection, if discovery is blocked
"""
import argparse, hashlib, json, os, platform, shutil, socket, subprocess, sys, threading, time, uuid

TCP_PORT, UDP_PORT = 47474, 47475
NODE, OS = uuid.uuid4().hex[:6], platform.system()
peers = {}              # node id -> (ip, port, last_seen)
known = []              # hashes of recent texts, to stop echo loops
lock = threading.Lock()

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(errors="replace")

def log(msg):
    t = time.time()
    print(time.strftime("%H:%M:%S", time.localtime(t)) + ".%03d" % (int(t * 1000) % 1000), msg, flush=True)

def h(s): return hashlib.sha1(s.encode("utf-8", "replace")).hexdigest()
def preview(s): return repr(s if len(s) <= 60 else s[:59] + "…")

def remember(*texts):
    with lock:
        for t in texts:
            if t is not None:
                known.append(h(t))
        del known[:-12]

# ---------- clipboard backends: (get, set, seq or None, label) ----------
def backend():
    if OS == "Darwin":
        env = dict(os.environ, LANG="en_US.UTF-8")
        get = lambda: subprocess.run(["pbpaste"], capture_output=True, env=env, timeout=2).stdout.decode("utf-8", "replace")
        def set_(t): subprocess.run(["pbcopy"], input=t.encode("utf-8"), env=env, timeout=2)
        return get, set_, None, "macOS pbpaste/pbcopy (polling)"

    if OS == "Windows":
        import ctypes
        from ctypes import wintypes
        u, k = ctypes.WinDLL("user32"), ctypes.WinDLL("kernel32")
        u.OpenClipboard.argtypes = [wintypes.HWND]; u.OpenClipboard.restype = wintypes.BOOL
        u.GetClipboardData.argtypes = [wintypes.UINT]; u.GetClipboardData.restype = wintypes.HANDLE
        u.SetClipboardData.argtypes = [wintypes.UINT, wintypes.HANDLE]; u.SetClipboardData.restype = wintypes.HANDLE
        u.EmptyClipboard.restype = wintypes.BOOL; u.CloseClipboard.restype = wintypes.BOOL
        u.GetClipboardSequenceNumber.restype = wintypes.DWORD
        k.GlobalAlloc.argtypes = [wintypes.UINT, ctypes.c_size_t]; k.GlobalAlloc.restype = wintypes.HGLOBAL
        k.GlobalLock.argtypes = [wintypes.HGLOBAL]; k.GlobalLock.restype = wintypes.LPVOID
        k.GlobalUnlock.argtypes = [wintypes.HGLOBAL]
        CF_UNICODETEXT = 13

        def opened():
            for _ in range(10):
                if u.OpenClipboard(None): return True
                time.sleep(0.03)
            return False
        def get():
            if not opened(): return ""
            try:
                handle = u.GetClipboardData(CF_UNICODETEXT)
                if not handle: return ""
                p = k.GlobalLock(handle)
                try: return ctypes.wstring_at(p)
                finally: k.GlobalUnlock(handle)
            finally: u.CloseClipboard()
        def set_(t):
            data = (t + "\0").encode("utf-16-le")
            handle = k.GlobalAlloc(0x0002, len(data))
            ctypes.memmove(k.GlobalLock(handle), data, len(data)); k.GlobalUnlock(handle)
            if not opened(): raise RuntimeError("clipboard busy")
            try: u.EmptyClipboard(); u.SetClipboardData(CF_UNICODETEXT, handle)
            finally: u.CloseClipboard()
        return get, set_, lambda: u.GetClipboardSequenceNumber(), "Windows clipboard API (change counter)"

    # Linux
    if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-paste"):
        g, s, label = ["wl-paste", "--no-newline"], ["wl-copy"], "Wayland wl-paste/wl-copy (polling)"
    elif shutil.which("xclip"):
        g, s, label = ["xclip", "-selection", "clipboard", "-o"], ["xclip", "-selection", "clipboard", "-i"], "X11 xclip (polling)"
    elif shutil.which("xsel"):
        g, s, label = ["xsel", "-b", "-o"], ["xsel", "-b", "-i"], "X11 xsel (polling)"
    else:
        sys.exit("Install wl-clipboard (Wayland) or xclip (X11) first, e.g. sudo apt install wl-clipboard xclip")
    def get():
        r = subprocess.run(g, capture_output=True, timeout=3)
        return r.stdout.decode("utf-8", "replace") if r.returncode == 0 else ""
    def set_(t): subprocess.run(s, input=t.encode("utf-8"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3)
    return get, set_, None, label

# ---------- network ----------
def my_ips():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.connect(("10.255.255.255", 1))
        ip = s.getsockname()[0]; s.close(); return ip
    except OSError: return "unknown"

def announce(token, manual):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    msg = f"UCSPIKE1 {token} {NODE} {TCP_PORT}".encode()
    while True:
        for dest in ["255.255.255.255"] + manual:
            try: s.sendto(msg, (dest, UDP_PORT))
            except OSError: pass
        time.sleep(2)

def listen_udp(token):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("", UDP_PORT))
    while True:
        data, (ip, _) = s.recvfrom(512)
        try: tag, tok, node, port = data.decode().split()
        except ValueError: continue
        if tag != "UCSPIKE1" or tok != token or node == NODE: continue
        with lock:
            new = node not in peers
            peers[node] = (ip, int(port), time.time())
        if new: log(f"FOUND PEER {node} at {ip} (discovery works on this network)")

def listen_tcp(token, set_):
    srv = socket.create_server(("", TCP_PORT))
    while True:
        conn, (ip, _) = srv.accept()
        try:
            conn.settimeout(3)
            buf = b""
            while not buf.endswith(b"\n") and len(buf) < 2_000_000:
                chunk = conn.recv(65536)
                if not chunk: break
                buf += chunk
            msg = json.loads(buf)
            if msg.get("token") != token or msg.get("node") == NODE: continue
            text = msg["text"]
            remember(text)
            set_(text)
            remember(text)  # again after the OS may have normalised it
            skew = int(time.time() * 1000 - msg.get("t", 0))
            log(f"<- RECEIVED from {msg.get('os')} {ip}: {preview(text)}  (wrote to clipboard; sender-to-receiver clock delta {skew} ms)")
        except Exception as e:
            log(f"receive error from {ip}: {e}")
        finally: conn.close()

def send(token, text, manual):
    with lock:
        targets = {ip: port for ip, port, seen in peers.values() if time.time() - seen < 12}
    for m in manual: targets.setdefault(m, TCP_PORT)
    if not targets: log(f"-> copied {preview(text)} but no peers known yet"); return
    payload = (json.dumps({"token": token, "node": NODE, "os": OS, "text": text, "t": int(time.time() * 1000)}) + "\n").encode()
    for ip, port in targets.items():
        t0 = time.time()
        try:
            with socket.create_connection((ip, port), timeout=2) as c: c.sendall(payload)
            log(f"-> SENT {preview(text)} to {ip} in {int((time.time() - t0) * 1000)} ms")
        except OSError as e:
            log(f"-> FAILED to reach {ip}:{port} ({e}). Firewall, or Wi-Fi client isolation?")

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--token", default="spike"); ap.add_argument("--peer", action="append", default=[])
    a = ap.parse_args()
    get, set_, seq, label = backend()
    log(f"node {NODE} on {OS}, this machine {my_ips()}, clipboard via {label}")
    if OS == "Linux": log(f"session type: {os.environ.get('XDG_SESSION_TYPE', 'unknown')}, desktop: {os.environ.get('XDG_CURRENT_DESKTOP', 'unknown')}")
    for fn, args in ((listen_tcp, (a.token, set_)), (listen_udp, (a.token,)), (announce, (a.token, a.peer))):
        threading.Thread(target=fn, args=args, daemon=True).start()
    try: remember(get())   # whatever is on the clipboard now is not a new copy
    except Exception: pass
    log("ready. Copy something on any machine. Ctrl+C to stop.")
    prev = None
    while True:
        try:
            s = seq() if seq else None
            if seq is None or s != prev:
                prev = s
                t = get()
                if t and h(t) not in known:
                    remember(t); log(f"<> LOCAL COPY detected: {preview(t)}"); send(a.token, t, a.peer)
        except Exception as e:
            log(f"clipboard error: {e}")
        time.sleep(0.25)

if __name__ == "__main__":
    try: main()
    except KeyboardInterrupt: print("\nstopped")

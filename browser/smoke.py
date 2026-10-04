"""Проверка собранного overnet browser через Marionette (без сторонних пакетов).

    python smoke.py PATH/overnet-browser.exe [SCREENSHOT_DIR] [--offline]

Запускает браузер на временном профиле и проверяет: название, что шлюз
поднялся и отдаёт browser.ov, что обычный сайт уходит на заглушку, что
открывается search.ov. Выход 1 — если что-то не так.
"""

import base64
import json
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path


class Marionette:
    def __init__(self, port=2828, timeout=60):
        end = time.time() + timeout
        while True:
            try:
                self.s = socket.create_connection(("127.0.0.1", port), timeout=5)
                break
            except OSError:
                if time.time() > end:
                    raise
                time.sleep(0.5)
        self.s.settimeout(90)
        self.buf = b""
        self.id = 0
        self._read()  # приветствие
        self.cmd("WebDriver:NewSession", {"capabilities": {}})

    def _read(self):
        while b":" not in self.buf:
            self.buf += self.s.recv(65536)
        n, rest = self.buf.split(b":", 1)
        n = int(n)
        while len(rest) < n:
            rest += self.s.recv(65536)
        self.buf = rest[n:]
        return json.loads(rest[:n])

    def cmd(self, name, params=None):
        self.id += 1
        msg = json.dumps([0, self.id, name, params or {}]).encode()
        self.s.sendall(str(len(msg)).encode() + b":" + msg)
        while True:
            r = self._read()
            if r[1] == self.id:
                if r[2]:
                    raise RuntimeError(f"{name}: {r[2]}")
                return r[3]

    def chrome(self, script):
        self.cmd("Marionette:SetContext", {"value": "chrome"})
        try:
            # Скрипт может ждать (await): Marionette дожидается возвращённого промиса.
            wrapped = "return (async () => {" + script + "})();"
            return self.cmd("WebDriver:ExecuteScript", {"script": wrapped, "args": []}).get("value")
        finally:
            self.cmd("Marionette:SetContext", {"value": "content"})


def main():
    exe = Path(sys.argv[1]).resolve()
    args = [a for a in sys.argv[2:] if not a.startswith("--")]
    shots = Path(args[0]) if args else None
    profile = tempfile.mkdtemp(prefix="ob-profile-")
    proc = subprocess.Popen([str(exe), "--marionette", "-remote-allow-system-access", "-profile", profile, "-no-remote"])
    ok = True

    def check(what, cond, detail=""):
        nonlocal ok
        ok &= bool(cond)
        print(("ok    " if cond else "FAIL  ") + what + (f": {detail}" if detail else ""))

    try:
        m = Marionette()
        brand = m.chrome("return Services.strings.createBundle('chrome://branding/locale/brand.properties').GetStringFromName('brandFullName')")
        check("brand name", brand == "overnet browser", brand)
        time.sleep(6)  # шлюз и первый опрос состояния
        state = m.chrome("const b=document.getElementById('overnet-button');return b&&!b.closest('[hidden]')?b.getAttribute('overnet-state'):'missing'")
        check("overnet indicator on the toolbar", state in ("ok", "connecting"), state)
        menu = m.chrome("PanelUI.show();await new Promise(r=>setTimeout(r,1500));const i=document.getElementById('appMenu-overnet-status');const l=i&&i.getAttribute('label');PanelUI.hide();return l||'missing'")
        check("overnet item in the app menu", menu.startswith("overnet:"), menu)
        panel = m.chrome("document.getElementById('overnet-button').click();await new Promise(r=>setTimeout(r,2500));const v=document.getElementById('PanelUI-overnet');const t=v?v.textContent+' | '+[...v.querySelectorAll('toolbarbutton')].map(b=>b.getAttribute('label')).join(', '):'missing';v&&v.closest('panel')?.hidePopup();return t")
        check("overnet panel", "Connected" in panel and "New circuits" in panel, panel[:200])

        def visit(url, settle=4):
            try:
                m.cmd("WebDriver:Navigate", {"url": url})
            except RuntimeError as e:
                return f"error {e}", ""
            time.sleep(settle)
            cur = m.cmd("WebDriver:GetCurrentURL")["value"]
            title = m.cmd("WebDriver:GetTitle")["value"]
            return cur, title

        def shot(name):
            if shots:
                shots.mkdir(parents=True, exist_ok=True)
                png = m.cmd("WebDriver:TakeScreenshot", {"full": False, "hash": False})["value"]
                (shots / f"{name}.png").write_bytes(base64.b64decode(png))

        cur, title = visit("http://browser.ov/", 6)
        check("start page browser.ov", title == "overnet browser", f"{cur} | {title}")
        shot("start")
        cur, title = visit("https://www.youtube.com/")
        check("youtube.com -> stub", cur.startswith("http://browser.ov/go") and "not an overnet site" in title, f"{cur} | {title}")
        shot("youtube")
        # На CI сеть overnet может быть недоступна: там .ov-сайты не проверяем.
        if "--offline" not in sys.argv:
            cur, title = visit("http://search.ov/", 15)
            check("search.ov opens", "search.ov" in title and "unavailable" not in title, f"{cur} | {title}")
            badge = m.chrome("for(let i=0;i<20&&!document.documentElement.hasAttribute('overnet-site');i++)await new Promise(r=>setTimeout(r,250));return document.documentElement.hasAttribute('overnet-site')")
            check("overnet badge in the address bar on .ov", badge)
            if shots:
                m.cmd("Marionette:SetContext", {"value": "chrome"})
                shot("chrome-search")
                m.cmd("Marionette:SetContext", {"value": "content"})
            shot("search")
        m.cmd("WebDriver:DeleteSession")
    except Exception as e:
        check("marionette", False, repr(e))
    finally:
        proc.terminate()
        try:
            proc.wait(15)
        except subprocess.TimeoutExpired:
            proc.kill()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()

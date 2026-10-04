"""Перепаковать Mullvad Browser (Windows) в overnet browser.

    python repack.py --mullvad DIR --overnet overnet.exe --out DIR [--rcedit rcedit.exe]

DIR — распакованный Mullvad Browser (папка с mullvadbrowser.exe). Сам Gecko не
пересобирается: меняются брендинг в omni.ja, настройки и политики, добавляются
autoconfig (overnet.cfg) и шлюз overnet. Защита от отпечатков Mullvad остаётся
как есть.
"""

import argparse
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import icons

HERE = Path(__file__).resolve().parent
FILES = HERE / "files"

NAME = "overnet browser"
EXE = "overnet-browser.exe"
MULLVAD_EXTENSION = "{d19a89b9-76c1-4a61-bcd4-49e8de916403}.xpi"
# Обновлять браузер будет overnet, а не апдейтер Mullvad; system-install
# переключил бы браузер с переносного профиля (рядом с ним) на общий.
DROP = ["updater.exe", "updater.ini", "update-settings.ini", "uninstall.exe", "postupdate.exe", "system-install"]

BRAND_FTL = f"""-brand-shorter-name = overnet
-brand-short-name = {NAME}
-brand-full-name = {NAME}
-brand-product-name = {NAME}
-vendor-short-name = overnet
trademarkInfo = {NAME} is built on Mullvad Browser and Firefox. It is not made or endorsed by Mullvad VPN AB or Mozilla.
"""
BRAND_PROPERTIES = f"brandShorterName=overnet\nbrandShortName={NAME}\nbrandFullName={NAME}\n"
WORDMARK_FTL = "mullvad-about-wordmark-en = OVERNET BROWSER\n"
BRANDING_CSS = """:root {
  --branding-gradient-start: #2a2d47;
  --branding-gradient-middle: #1d1f33;
  --branding-gradient-end: #161826;
  --branding-focus-outline-color: #9184d9;
  --branding-link-color: #b5abfc;
  --branding-link-color-hover: #d2cefd;
  --branding-link-color-active: #e4e1fe;
}
"""
WORDMARK_SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 60" width="420" height="60">
<text x="0" y="44" font-family="Inter, 'Segoe UI', system-ui, sans-serif" font-size="44" font-weight="500" fill="#e9e9ed">overnet browser</text>
</svg>
"""

PREF_FILES = ("defaults/preferences/000-mullvad-browser.js", "defaults/preferences/001-base-profile.js")

# Остатки имени в строках интерфейса (в переводах тоже), кроме brand.ftl,
# который переписывается целиком.
TEXT_SUFFIXES = (".ftl", ".properties", ".dtd")
RENAMES = [
    ("Mullvad Browser", NAME),
    ("Браузер Mullvad", "Браузер overnet"),
    ("Браузера Mullvad", "Браузера overnet"),
    ("браузера Mullvad", "браузера overnet"),
]


def rewrite_jar(path: Path, change) -> int:
    """Переписать omni.ja: change(name, data) -> новые данные или None (как было)."""
    src = zipfile.ZipFile(path)
    tmp = path.with_suffix(".new")
    changed = 0
    with zipfile.ZipFile(tmp, "w") as out:
        for info in src.infolist():
            data = src.read(info)
            new = change(info.filename, data)
            if new is not None and new != data:
                data = new
                changed += 1
            ni = zipfile.ZipInfo(info.filename, info.date_time)
            ni.compress_type = info.compress_type
            ni.external_attr = info.external_attr
            out.writestr(ni, data)
        for name, data in change.extra().items():
            out.writestr(zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0)), data, zipfile.ZIP_DEFLATED)
            changed += 1
    src.close()
    tmp.replace(path)
    return changed


def rename_text(data: bytes) -> bytes:
    text = data.decode("utf-8")
    for a, b in RENAMES:
        text = text.replace(a, b)
    return text.encode("utf-8")


class BrowserJar:
    """Изменения browser/omni.ja: брендинг, картинки, настройки."""

    def __init__(self, ic: Path):
        png = lambda s: (ic / f"icon{s}.png").read_bytes()
        svg = (ic / "overnet.svg").read_bytes()
        b = "chrome/browser/content/branding/"
        self.files = {
            **{f"{b}icon{s}.png": png(s) for s in (16, 32, 48, 64, 128, 256)},
            f"{b}about-logo.png": png(256),
            f"{b}about-logo@2x.png": png(512),
            f"{b}about.png": png(256),
            f"{b}about-logo.svg": svg,
            f"{b}about-wordmark.svg": WORDMARK_SVG.encode(),
            f"{b}firefox-wordmark.svg": WORDMARK_SVG.encode(),
            f"{b}document.ico": (ic / "overnet.ico").read_bytes(),
            f"{b}mullvad-branding.css": BRANDING_CSS.encode(),
            "chrome/devtools/skin/images/aboutdebugging-mullvadbrowser-logo.svg": svg,
        }

    def __call__(self, name: str, data: bytes):
        if name in self.files:
            return self.files[name]
        if name in PREF_FILES:
            # Порядок загрузки файлов настроек не алфавитный: дописываем свои в
            # конец каждого файла, где Mullvad задаёт то же самое.
            return data + b"\n" + (FILES / "overnet-prefs.js").read_bytes()
        if re.fullmatch(r"localization/[^/]+/branding/brand\.ftl", name):
            return BRAND_FTL.encode()
        if re.fullmatch(r"localization/[^/]+/branding/mullvad-about-wordmark-en\.ftl", name):
            return WORDMARK_FTL.encode()
        if re.fullmatch(r"chrome/[^/]+/locale/branding/brand\.properties", name):
            return BRAND_PROPERTIES.encode()
        if name.endswith(TEXT_SUFFIXES):
            return rename_text(data)
        return None

    def extra(self):
        return {}


class ToolkitJar:
    """omni.ja уровня GRE: только остатки имени в строках."""

    def __call__(self, name: str, data: bytes):
        return rename_text(data) if name.endswith(TEXT_SUFFIXES) else None

    def extra(self):
        return {}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mullvad", required=True, type=Path)
    ap.add_argument("--overnet", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--rcedit", type=Path)
    a = ap.parse_args()

    if not (a.mullvad / "mullvadbrowser.exe").exists():
        sys.exit(f"{a.mullvad}: no mullvadbrowser.exe here")
    if a.out.exists():
        shutil.rmtree(a.out)
    shutil.copytree(a.mullvad, a.out)
    out = a.out
    ic = out / "_icons"
    icons.main(ic)

    for f in DROP:
        (out / f).unlink(missing_ok=True)
    (out / "distribution" / "extensions" / MULLVAD_EXTENSION).unlink(missing_ok=True)

    (out / "mullvadbrowser.exe").rename(out / EXE)
    manifest = (out / "mullvadbrowser.VisualElementsManifest.xml").read_text(encoding="utf-8")
    (out / "mullvadbrowser.VisualElementsManifest.xml").unlink()
    (out / EXE.replace(".exe", ".VisualElementsManifest.xml")).write_text(
        manifest.replace("#192e45", "#161826"), encoding="utf-8"
    )
    for s in (70, 150):
        shutil.copy(ic / f"icon{s}.png", out / "browser" / "VisualElements" / f"VisualElements_{s}.png")

    shutil.copy(FILES / "policies.json", out / "distribution" / "policies.json")
    shutil.copy(FILES / "autoconfig.js", out / "defaults" / "pref" / "autoconfig.js")
    shutil.copy(FILES / "overnet.cfg", out / "overnet.cfg")
    (out / "overnet").mkdir(exist_ok=True)
    shutil.copy(a.overnet, out / "overnet" / a.overnet.name)

    n = rewrite_jar(out / "browser" / "omni.ja", BrowserJar(ic))
    m = rewrite_jar(out / "omni.ja", ToolkitJar())
    print(f"omni.ja: {n} files changed in browser/, {m} in the toolkit")

    if a.rcedit:
        subprocess.run(
            [
                str(a.rcedit), str(out / EXE),
                "--set-icon", str(ic / "overnet.ico"),
                "--set-version-string", "ProductName", NAME,
                "--set-version-string", "FileDescription", NAME,
                "--set-version-string", "CompanyName", "overnet",
                "--set-version-string", "InternalName", "overnet-browser",
                "--set-version-string", "OriginalFilename", EXE,
            ],
            check=True,
        )
    shutil.rmtree(ic)
    print(f"{NAME}: {out / EXE}")


if __name__ == "__main__":
    main()

"""overnet browser for Android: Firefox for Android (Fenix) with overnet inside.

Patches a Fenix checkout (mobile/android/fenix of the Firefox repository, the
`release` branch) the way browser/repack.py repackages Mullvad Browser on the
desktop: the browser stays Mozilla's, we add the gateway and the rules.

- package com.ospab.overnet, the name "overnet browser", the overnet icon, and
  its own sharedUserId and link scheme, so it installs next to Firefox;
- no telemetry and no crash reports to Mozilla;
- Gecko gets its settings from com.ospab.overnet.Overnet (SOCKS5 to the
  gateway inside the app, no direct fallback, no WebRTC);
- every page load first goes through Overnet.intercept: only .ov opens, a
  regular site becomes the browser.ov/go stub, searches go to search.ov.

Every replacement must match exactly once: when Mozilla changes the code, the
build stops here with the place to update instead of producing a browser that
quietly leaks.

    python patch.py FENIX_DIR
"""

import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def read(path: Path) -> str:
    # newline="": line endings stay as they are in Mozilla's files.
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


def write(path: Path, s: str) -> None:
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(s)


def sub(path: Path, old: str, new: str, count: int = 1) -> None:
    s = read(path)
    n = s.count(old)
    if n != count:
        sys.exit(f"patch: {path}: expected {count} of {old!r}, found {n}")
    write(path, s.replace(old, new))


def main() -> None:
    fenix = Path(sys.argv[1]).resolve()
    app = fenix / "app"
    gradle = app / "build.gradle"

    # Identity: com.ospab.overnet, installable next to Firefox.
    sub(gradle, 'applicationId "org.mozilla"', 'applicationId "com.ospab"')
    sub(gradle, '            applicationIdSuffix ".firefox"\n', '            applicationIdSuffix ".overnet"\n')
    s = read(gradle)
    start = s.index("        release releaseTemplate >> {")
    end = s.index("        benchmark releaseTemplate >> {", start)
    block = s[start:end]
    for old, new in (
        ('def deepLinkSchemeValue = "fenix"', 'def deepLinkSchemeValue = "overnet"'),
        ('"sharedUserId": "org.mozilla.firefox.sharedID"', '"sharedUserId": "com.ospab.overnet.sharedID"'),
    ):
        if block.count(old) != 1:
            sys.exit(f"patch: {gradle}: release block: expected one {old!r}")
        block = block.replace(old, new)
    write(gradle, s[:start] + block + s[end:])

    # Nothing goes to Mozilla's telemetry and crash servers.
    sub(gradle, "    buildConfigField 'boolean', 'CRASH_REPORTING', 'true'\n", "    buildConfigField 'boolean', 'CRASH_REPORTING', 'false'\n")
    sub(gradle, "    buildConfigField 'boolean', 'TELEMETRY', 'true'\n", "    buildConfigField 'boolean', 'TELEMETRY', 'false'\n")

    # The gateway and Gecko's settings.
    provider = app / "src/main/java/org/mozilla/fenix/gecko/GeckoProvider.kt"
    sub(
        provider,
        "            GeckoRuntimeSettings.Builder()\n                .crashHandler(",
        "            GeckoRuntimeSettings.Builder()\n"
        "                .configFilePath(com.ospab.overnet.Overnet.configFile(context))\n"
        "                .crashHandler(",
    )

    # Only .ov opens here.
    interceptor = app / "src/main/java/org/mozilla/fenix/AppRequestInterceptor.kt"
    sub(
        interceptor,
        "    ): RequestInterceptor.InterceptionResponse? {\n        interceptErrorPageAction(uri)?.let {",
        "    ): RequestInterceptor.InterceptionResponse? {\n"
        "        com.ospab.overnet.Overnet.intercept(context, uri, isSubframeRequest)?.let {\n"
        "            return it\n"
        "        }\n\n"
        "        interceptErrorPageAction(uri)?.let {",
    )

    # Our code, kept whole by R8: the gateway is called by name over JNI.
    src = app / "src/main/java/com/ospab/overnet"
    src.mkdir(parents=True, exist_ok=True)
    for f in (HERE / "src/com/ospab/overnet").glob("*.kt"):
        shutil.copy(f, src / f.name)
    rules = app / "proguard-rules.pro"
    write(rules, read(rules) + "\n# overnet: JNI\n-keep class com.ospab.overnet.Gateway { *; }\n")

    # Name and icon. The release icon's density PNGs stay for Android 7; from 8
    # on the adaptive icon below wins.
    sub(
        app / "src/release/res/values/static_strings.xml",
        '<string name="app_name" translatable="false">Firefox</string>',
        '<string name="app_name" translatable="false">overnet browser</string>',
    )
    res = app / "src/release/res"
    for f in (HERE / "res").rglob("*.xml"):
        dest = res / f.relative_to(HERE / "res")
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(f, dest)

    print(f"patched {fenix}")


if __name__ == "__main__":
    main()

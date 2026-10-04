"""Знак overnet — кольцо в кольце, как `.mark` в шапке сайтов — для браузера.

Рисует PNG нужных размеров, ICO (PNG внутри) и SVG без сторонних библиотек:
сборка на GitHub Actions не должна ничего ставить ради иконки.

    python icons.py OUT_DIR
"""

import struct
import sys
import zlib
from pathlib import Path

BG = (0x16, 0x18, 0x26)      # --color-bg дизайн-системы
ACCENT = (0x91, 0x84, 0xd9)  # --accent

# Доли от размера иконки: фон-круг, внешнее и внутреннее кольцо.
BG_R = 0.50
OUTER_R, OUTER_W = 0.36, 0.075
INNER_R, INNER_W = 0.155, 0.075

SVG = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
<circle cx="50" cy="50" r="{BG_R * 100}" fill="#{''.join(f'{c:02x}' for c in BG)}"/>
<circle cx="50" cy="50" r="{OUTER_R * 100}" fill="none" stroke="#{''.join(f'{c:02x}' for c in ACCENT)}" stroke-width="{OUTER_W * 100}"/>
<circle cx="50" cy="50" r="{INNER_R * 100}" fill="none" stroke="#{''.join(f'{c:02x}' for c in ACCENT)}" stroke-width="{INNER_W * 100}"/>
</svg>
"""


def coverage(d: float, r: float, w: float) -> float:
    """Доля пикселя на расстоянии d от центра, попавшая в кольцо (r, ширина w)."""
    return max(0.0, min(1.0, w / 2 - abs(d - r) + 0.5))


def render(size: int) -> bytes:
    """RGBA-пиксели знака size×size, со сглаживанием по расстоянию."""
    c = size / 2
    rows = []
    for y in range(size):
        row = bytearray([0])  # фильтр строки PNG: None
        for x in range(size):
            d = ((x + 0.5 - c) ** 2 + (y + 0.5 - c) ** 2) ** 0.5
            bg = max(0.0, min(1.0, BG_R * size - d + 0.5))
            ring = max(coverage(d, OUTER_R * size, OUTER_W * size), coverage(d, INNER_R * size, INNER_W * size))
            px = [round(BG[i] * (1 - ring) + ACCENT[i] * ring) for i in range(3)]
            row += bytes(px) + bytes([round(255 * max(bg, ring))])
        rows.append(bytes(row))
    return b"".join(rows)


def png(size: int) -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(render(size), 9)) + chunk(b"IEND", b"")


def ico(images: dict) -> bytes:
    """ICO с PNG внутри (так умеют все Windows с Vista)."""
    sizes = sorted(images)
    head = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    entries, blobs = b"", b""
    for s in sizes:
        data = images[s]
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset + len(blobs))
        blobs += data
    return head + entries + blobs


def main(out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    images = {s: png(s) for s in (16, 24, 32, 48, 64, 70, 128, 150, 256, 512)}
    for s, data in images.items():
        (out / f"icon{s}.png").write_bytes(data)
    (out / "overnet.ico").write_bytes(ico({s: images[s] for s in (16, 24, 32, 48, 64, 128, 256)}))
    (out / "overnet.svg").write_text(SVG, encoding="utf-8")


if __name__ == "__main__":
    main(Path(sys.argv[1] if len(sys.argv) > 1 else "icons"))

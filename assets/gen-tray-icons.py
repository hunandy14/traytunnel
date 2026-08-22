#!/usr/bin/env python3
"""系統匣小尺寸圖層產生器：把簡化變體做「像素對齊」（pixel hinting）後光柵化。

背景
----
先前 16/20/24/28/32 這五層是拿 `assets/icon-final-16-simplified.svg`（256 視框）
直接等比縮小光柵化的產物，幾何邊界落在非整數像素上，每一條直邊都被抗鋸齒抹成
兩排半透明像素，在 175% DPI（系統匣取 28px 層）看起來就是「軟」。

再來，這幾層原本還墊了一塊深色圓角底板。底板在淺色工作列上會變成一顆突兀的黑
方塊，而且吃掉一圈邊距讓盾牌只剩畫布的七成。現在底板拿掉了：小尺寸層的背景是
全透明，只留盾牌 glyph（盾內的深色通道環是圖形本體，保留），釋放出來的邊距全
數換成盾牌尺寸——盾高直接吃滿畫布，只留 1px 安全邊。

作法
----
本腳本不縮放 SVG，而是「為每個目標尺寸重算一次幾何」，把關鍵邊界四捨五入到整數
像素格線上：

- 盾形的左右垂直側邊（xL／xR）、肩線、直邊結束線、頂點與底尖
- 同心圓的圓心（落在整數格點，左右對稱）與內外半徑（整數，直徑正好占滿整數像素）

只有無法避免的曲線（斜肩線、盾底貝茲、圓弧）才留抗鋸齒；垂直邊一律實心整像素。
光柵化用「x 方向解析、y 方向超取樣」的面積覆蓋率，再對覆蓋率做一次輕微的斜率
銳化（EDGE_GAIN）。EDGE_GAIN 是這份檔案裡最敏感的旋鈕：

- 1.6（舊值）＝過渡帶壓到不足半像素，28px 全圖只剩 16 個半透明像素，斜肩線與盾底
  冒出生硬的階梯
- 1.0＝不銳化的純面積平均，過渡帶滿一像素寬，28px 有 60 個半透明像素，邊緣發虛
- 1.15（現值）＝28px 42 個過渡像素，落在上面兩者中間；垂直邊仍是硬邊（幾何本來就
  對齊格線，覆蓋率非 0 即 1），斜邊與圓弧各留一排面積覆蓋率，鋸齒消失但邊不虛

輸出
----
- assets/icon-tray-{16,20,24,28,32}.svg：各尺寸對齊後的幾何（人可讀的定稿記錄）
- src-tauri/icons/icon.ico、traytunnel.ico 的這五層（就地換位元組，48px 以上不動）

用法：python assets/gen-tray-icons.py [--png-out DIR]
"""

from __future__ import annotations

import argparse
import math
import struct
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SIZES = (16, 20, 24, 28, 32)
ICO_TARGETS = ("src-tauri/icons/icon.ico", "traytunnel.ico")

# 定稿簡化變體的色票（assets/icon-final-16-simplified.svg）
C_SHIELD = (0x2D, 0xD4, 0xA7)  # 盾身
C_HOLE = (0x0E, 0x10, 0x13)  # 通道負空間
C_NODE = (0x4B, 0xF0, 0xC7)  # 中心節點

# 原始 256 視框裡盾形的外框：x 56..200、y 30..226。以下比例一律換算成
# 「盾形自身的外接框」——寬 W0=144、高 H0=196——好讓盾牌能獨立縮放去吃滿畫布。
W0, H0 = 144.0, 196.0
F_ASPECT = W0 / H0  # 盾寬／盾高
F_SHOULDER = 28 / H0  # 肩線（左右垂直側邊起點），距頂點
F_STRAIGHT = 128 / H0  # 垂直側邊結束，距頂點
F_C1Y = 160 / H0  # 盾底貝茲第一控制點，距頂點
F_C2X = 112 / W0  # 盾底貝茲第二控制點，距左側邊
F_C2Y = 180 / H0
F_RING_CY = 90 / H0  # 同心圓圓心，距頂點
F_RING_R = 54 / W0  # 外圈（通道）半徑，對盾寬
F_NODE_R = 26 / W0  # 中心節點半徑，對盾寬

MARGIN = 1  # 四邊安全邊（像素）
SSY = 16  # y 方向每像素的超取樣列數
EDGE_GAIN = 1.15  # 覆蓋率斜率銳化：把 AA 過渡帶壓到約 1/1.15 像素寬


# ---------------------------------------------------------------- 幾何


def geometry(n: int) -> dict:
    """算出尺寸 n 的像素對齊幾何。整數欄位代表「必須落在像素格線上」的邊界。"""
    apex = MARGIN
    bottom = n - MARGIN
    h = bottom - apex  # 盾高吃滿畫布（扣掉安全邊）

    # 盾寬照原比例推，再把左右側邊各自吸到整數格線；n 皆為偶數，鏡射即左右對稱
    w_ideal = h * F_ASPECT
    x_l = round((n - w_ideal) / 2)
    x_r = n - x_l
    w = x_r - x_l
    cx = n / 2

    # 半徑取整＝直徑為偶數，配上落在格點的圓心，圓的外接框剛好占滿整數像素。
    # 半徑照「理想盾寬」而非取整後的盾寬推算，免得 x_l 的捨入誤差被放大到環上。
    ring_r = max(1, round(w_ideal * F_RING_R))
    node_r = max(1, round(w_ideal * F_NODE_R))
    ring_cy = apex + round(h * F_RING_CY)
    return {
        "n": n,
        "shield": {
            "x_l": x_l,
            "x_r": x_r,
            "w": w,
            "cx": cx,
            "apex": apex,
            "shoulder": apex + round(h * F_SHOULDER),
            "straight": apex + round(h * F_STRAIGHT),
            "bottom": bottom,
            # 曲線控制點不需對齊，照比例縮放即可
            "c1y": apex + h * F_C1Y,
            "c2x": x_l + w * F_C2X,
            "c2y": apex + h * F_C2Y,
        },
        "ring": (cx, ring_cy, ring_r),
        "node": (cx, ring_cy, node_r),
    }


def shield_polygon(g: dict, steps: int = 64) -> list[tuple[float, float]]:
    """盾形外框攤平成多邊形（底部兩段三次貝茲各切 steps 段）。"""
    s = g["shield"]
    n = g["n"]
    cx, x_l, x_r = s["cx"], s["x_l"], s["x_r"]
    pts = [
        (cx, float(s["apex"])),
        (float(x_r), float(s["shoulder"])),
        (float(x_r), float(s["straight"])),
    ]

    def cubic(p0, p1, p2, p3):
        for i in range(1, steps + 1):
            t = i / steps
            u = 1 - t
            yield (
                u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1],
            )

    c2x_r = s["c2x"]
    c2x_l = n - c2x_r  # 盾牌以 cx = n/2 為軸，鏡射即得左側控制點
    pts += list(
        cubic(
            (x_r, s["straight"]),
            (x_r, s["c1y"]),
            (c2x_r, s["c2y"]),
            (cx, s["bottom"]),
        )
    )
    pts += list(
        cubic(
            (cx, s["bottom"]),
            (c2x_l, s["c2y"]),
            (x_l, s["c1y"]),
            (x_l, s["straight"]),
        )
    )
    pts.append((float(x_l), float(s["shoulder"])))
    return pts


# ---------------------------------------------------------------- 光柵化


def spans_circle(circle, y):
    """直徑大於 6px 用真圓（曲線的 AA 無法避免），6px 以下改走手繪像素圓盤。"""
    cx, cy, r = circle
    if r <= 3:
        return spans_pixel_disc(circle, y)
    dy = y - cy
    if abs(dy) >= r:
        return ()
    dx = math.sqrt(r * r - dy * dy)
    return ((cx - dx, cx + dx),)


# 極小圓盤的手繪半寬表（直徑 2/4/6），列出每個像素列的半寬度。
# 2px、4px 一律實心方塊——這個尺度的「圓」畫成八角形只會變成一個十字，
# 實心方塊反而最像一顆點；6px 才開始削掉四個角。
PIXEL_DISC = {
    1: (1, 1),
    2: (2, 2, 2, 2),
    3: (2, 3, 3, 3, 3, 2),
}


def spans_pixel_disc(circle, y):
    cx, cy, r = circle
    j = math.floor(y) - (cy - r)
    table = PIXEL_DISC[r]
    if j < 0 or j >= len(table):
        return ()
    h = table[j]
    return ((cx - h, cx + h),)


def spans_polygon(pts, y):
    xs = []
    m = len(pts)
    for i in range(m):
        x0, y0 = pts[i]
        x1, y1 = pts[(i + 1) % m]
        if (y0 <= y < y1) or (y1 <= y < y0):
            xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
    xs.sort()
    return tuple(zip(xs[0::2], xs[1::2]))


def coverage(n: int, span_fn) -> list[list[float]]:
    """x 方向解析、y 方向超取樣的面積覆蓋率，再做一次邊緣斜率銳化。"""
    cov = [[0.0] * n for _ in range(n)]
    w = 1.0 / SSY
    for py in range(n):
        row = cov[py]
        for i in range(SSY):
            y = py + (i + 0.5) / SSY
            for x0, x1 in span_fn(y):
                if x1 <= x0:
                    continue
                x0 = max(0.0, x0)
                x1 = min(float(n), x1)
                if x1 <= x0:
                    continue
                px0, px1 = int(x0), min(n - 1, int(math.ceil(x1)) - 1)
                for px in range(px0, px1 + 1):
                    left = max(x0, px)
                    right = min(x1, px + 1.0)
                    if right > left:
                        row[px] += (right - left) * w
    for row in cov:
        for px in range(n):
            c = row[px]
            if 0.0 < c < 1.0:
                row[px] = min(1.0, max(0.0, (c - 0.5) * EDGE_GAIN + 0.5))
    return cov


def render(n: int) -> bytearray:
    """回傳 top-down 的 RGBA 位元組（背景全透明）。"""
    g = geometry(n)
    poly = shield_polygon(g)
    layers = [
        (C_SHIELD, coverage(n, lambda y, p=poly: spans_polygon(p, y))),
        (C_HOLE, coverage(n, lambda y, c=g["ring"]: spans_circle(c, y))),
        (C_NODE, coverage(n, lambda y, c=g["node"]: spans_circle(c, y))),
    ]

    buf = bytearray(n * n * 4)
    for py in range(n):
        for px in range(n):
            # 以預乘色做 source-over，最後再還原
            pr = pg = pb = pa = 0.0
            for (sr, sg, sb), cov in layers:
                c = cov[py][px]
                if c <= 0.0:
                    continue
                inv = 1.0 - c
                pr = sr * c + pr * inv
                pg = sg * c + pg * inv
                pb = sb * c + pb * inv
                pa = c + pa * inv
            o = (py * n + px) * 4
            if pa <= 0.0:
                continue
            buf[o] = min(255, round(pr / pa))
            buf[o + 1] = min(255, round(pg / pa))
            buf[o + 2] = min(255, round(pb / pa))
            buf[o + 3] = min(255, round(pa * 255))
    return buf


# ---------------------------------------------------------------- 輸出


def disc_shape(circle, fill: str) -> str:
    """把圓輸出成 SVG：直徑 6px 以下走手繪像素圓盤，得畫成矩形集合才對得上光柵。"""
    cx, cy, r = circle
    if r > 3:
        return f'<circle cx="{cx:g}" cy="{cy}" r="{r}" fill="{fill}"/>'
    table = PIXEL_DISC[r]
    rects, j = [], 0
    while j < len(table):
        k = j
        while k + 1 < len(table) and table[k + 1] == table[j]:
            k += 1
        h = table[j]
        rects.append(f"M{cx - h:g},{cy - r + j} h{2 * h} v{k - j + 1} h{-2 * h} z")
        j = k + 1
    return f'<path d="{" ".join(rects)}" fill="{fill}"/>'


def to_svg(n: int) -> str:
    g = geometry(n)
    s = g["shield"]
    fmt = lambda v: f"{v:g}"
    path = (
        f"M{fmt(s['cx'])},{s['apex']} L{s['x_r']},{s['shoulder']} V{s['straight']} "
        f"C{s['x_r']},{fmt(round(s['c1y'], 3))} {fmt(round(s['c2x'], 3))},{fmt(round(s['c2y'], 3))} "
        f"{fmt(s['cx'])},{s['bottom']} "
        f"C{fmt(round(n - s['c2x'], 3))},{fmt(round(s['c2y'], 3))} {s['x_l']},{fmt(round(s['c1y'], 3))} "
        f"{s['x_l']},{s['straight']} V{s['shoulder']} Z"
    )
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}">\n'
        f"  <!-- {n}px 系統匣圖層：assets/icon-final-16-simplified.svg 的像素對齊版，\n"
        f"       由 assets/gen-tray-icons.py 產生。背景透明、垂直邊落在整數像素格線上，請勿手改。 -->\n"
        f'  <path d="{path}" fill="#2dd4a7"/>\n'
        f"  {disc_shape(g['ring'], '#0e1013')}\n"
        f"  {disc_shape(g['node'], '#4bf0c7')}\n"
        f"</svg>\n"
    )


def write_png(path: Path, n: int, rgba: bytes) -> None:
    raw = b"".join(b"\x00" + bytes(rgba[y * n * 4 : (y + 1) * n * 4]) for y in range(n))

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def dib_layer(n: int, rgba: bytes) -> bytes:
    """組出 ICO 用的 32bpp BMP 圖層：BITMAPINFOHEADER + 由下而上的 BGRA（無 AND 遮罩）。"""
    head = struct.pack("<IiiHHIIiiII", 40, n, n * 2, 1, 32, 0, n * n * 4, 0, 0, 0, 0)
    rows = [rgba[y * n * 4 : (y + 1) * n * 4] for y in range(n)]
    body = bytearray()
    for row in reversed(rows):
        for px in range(n):
            r, g, b, a = row[px * 4 : px * 4 + 4]
            body += bytes((b, g, r, a))
    return head + bytes(body)


def patch_ico(path: Path, layers: dict[int, bytes]) -> list[int]:
    data = bytearray(path.read_bytes())
    count = struct.unpack("<H", data[4:6])[0]
    patched = []
    for i in range(count):
        e = 6 + i * 16
        w = data[e] or 256
        h = data[e + 1] or 256
        size, off = struct.unpack("<II", data[e + 8 : e + 16])
        if w != h or w not in layers:
            continue
        blob = dib_layer(w, layers[w])
        if len(blob) != size:
            raise SystemExit(f"{path.name} 的 {w}px 圖層長度不符：{len(blob)} != {size}")
        data[off : off + size] = blob
        patched.append(w)
    path.write_bytes(bytes(data))
    return patched


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--png-out", type=Path, help="另外把各尺寸存成 PNG（目檢用）")
    ap.add_argument("--no-ico", action="store_true", help="只產 SVG／PNG，不改 ico")
    args = ap.parse_args()

    rasters = {}
    for n in SIZES:
        raster = bytes(render(n))
        rasters[n] = raster
        (REPO / "assets" / f"icon-tray-{n}.svg").write_text(to_svg(n), encoding="utf-8")
        if args.png_out:
            args.png_out.mkdir(parents=True, exist_ok=True)
            write_png(args.png_out / f"hinted-{n}.png", n, raster)
        g = geometry(n)
        s = g["shield"]
        aa = sum(1 for i in range(n * n) if 0 < raster[i * 4 + 3] < 255)
        print(
            f"  {n}px 盾 x {s['x_l']}..{s['x_r']} y {s['apex']}..{s['bottom']}"
            f" 環 r={g['ring'][2]} 節點 r={g['node'][2]} AA 過渡像素 {aa}"
        )

    if not args.no_ico:
        for rel in ICO_TARGETS:
            p = REPO / rel
            print(f"  {rel} 換掉圖層 {patch_ico(p, rasters)}")


if __name__ == "__main__":
    main()

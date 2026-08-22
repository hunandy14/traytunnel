#!/usr/bin/env python3
"""Traytunnel 圖示工廠：一套幾何、一顆圖示，產出全平台全尺寸的資產。

為什麼是一支腳本
----------------
這顆圖示以前有兩個來源：`assets/icon-final.svg`（256 完整版，帶深色圓角底板、
teal 漸層）負責 48px 以上的層與各種 PNG；`assets/icon-final-16-simplified.svg`
（去漸層的簡化變體）負責系統匣的小層。兩份 SVG 各自演化，結果就是同一支程式在
系統匣、工作列、開始選單裡長得不一樣——大尺寸有一塊黑底板，小尺寸沒有。

現在只剩這支腳本。盾牌幾何（比例、圓環、節點）寫死在下面的 F_* 常數裡，所有尺寸
都從同一組常數重算，一律透明底、盾牌吃滿畫布只留安全邊。兩份 SVG 退居下游產物：
`assets/icon-final.svg` 是大尺寸外觀的文件參考，`public/icon.svg` 是標題列與
favicon 的向量版，兩者都由本腳本重寫，改幾何請改這裡。

兩種光柵化
----------
**像素對齊（hinting）**，用在 16/20/24/28/32 這五層：不縮放向量，而是為每個尺寸
重算幾何，把關鍵邊界（盾形左右垂直側邊、肩線、直邊結束線、圓心與內外半徑）四捨
五入到整數像素格線上，只有無法避免的曲線才留抗鋸齒。覆蓋率再過一次斜率銳化
（EDGE_GAIN）：

- 1.6（舊值）＝過渡帶壓到不足半像素，28px 全圖只剩 16 個半透明像素，斜肩線與盾底
  冒出生硬的階梯
- 1.0＝不銳化的純面積平均，過渡帶滿一像素寬，28px 有 60 個半透明像素，邊緣發虛
- 1.15（現值）＝28px 42 個過渡像素，落在上面兩者中間；垂直邊仍是硬邊（幾何本來就
  對齊格線，覆蓋率非 0 即 1），斜邊與圓弧各留一排面積覆蓋率，鋸齒消失但邊不虛

**平滑渲染**，用在 48px 以上（以及 30/44/50/71/89/107/142/150/284/310 這些 appx
非整數尺寸）：幾何不吸格線，直接用高倍超取樣（x 方向解析面積、y 方向 SSY_SMOOTH
列）光柵化，不做邊緣銳化，盾身與節點填 icon-final.svg 那條對角 teal 漸層。這個尺
度下像素對齊沒有意義，反而是漸層質感撐得起場面。

兩條路的分界就是「像素多到不需要 hint」的那一刻——32px 的安全邊是 1px，等於 n/32，
平滑路徑的 MARGIN_RATIO 就取 1/32，讓 32 與 48 之間的盾牌大小連續接得上。

產物
----
- src-tauri/icons/icon.ico、traytunnel.ico：16/20/24/28/32/48/64/128/256 九層
- src-tauri/icons/*.png：Tauri bundler 與 appx 要的各尺寸（清單見 PNG_TARGETS）
- src-tauri/icons/icon.icns：PNG-based icns（ic11/ic12/ic07/ic13/ic08/ic14/ic09/ic10）
- assets/icon-tray-{16,20,24,28,32}.svg：對齊後的小尺寸幾何（人可讀的定稿記錄）
- assets/icon-final.svg：256 無底板漸層版（文件參考）
- public/icon.svg：標題列（16 CSS px）與 favicon 的向量版，走小尺寸的實色配色

用法：python assets/gen-tray-icons.py [--png-out DIR]
"""

from __future__ import annotations

import argparse
import math
import struct
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# 走像素對齊的尺寸；其餘一律走平滑渲染
SIZES = (16, 20, 24, 28, 32)
ICO_SIZES = (16, 20, 24, 28, 32, 48, 64, 128, 256)
ICO_TARGETS = ("src-tauri/icons/icon.ico", "traytunnel.ico")

# Tauri bundler（tauri.conf.json 的 icon 清單）＋ Windows appx 的方形圖磚
PNG_TARGETS = {
    "32x32.png": 32,
    "64x64.png": 64,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}

# icns 的 PNG-based 型別：1x 是 ic07/ic08/ic09/ic10，2x 是 ic11/ic12/ic13/ic14
ICNS_ENTRIES = (
    (b"ic11", 32),  # 16@2x
    (b"ic12", 64),  # 32@2x
    (b"ic07", 128),
    (b"ic13", 256),  # 128@2x
    (b"ic08", 256),
    (b"ic14", 512),  # 256@2x
    (b"ic09", 512),
    (b"ic10", 1024),  # 512@2x
)

# 小尺寸的實色色票（歷史來源 assets/icon-final-16-simplified.svg）
C_SHIELD = (0x2D, 0xD4, 0xA7)  # 盾身
C_HOLE = (0x0E, 0x10, 0x13)  # 通道負空間
C_NODE = (0x4B, 0xF0, 0xC7)  # 中心節點

# 大尺寸的對角 teal 漸層（歷史來源 icon-final.svg 的 linearGradient#teal，
# objectBoundingBox 的 (0,0)→(1,1)）。盾身與節點各自套在自己的外接框上。
GRAD = ((0x4B, 0xF0, 0xC7), (0x1F, 0xA3, 0x85))

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

MARGIN = 1  # 像素對齊路徑的四邊安全邊（像素）
MARGIN_RATIO = 1 / 32  # 平滑路徑的安全邊比例，在 32px 上剛好等於 MARGIN
SSY = 16  # 像素對齊路徑：y 方向每像素的超取樣列數
SSY_SMOOTH = 32  # 平滑路徑：曲線多，超取樣加倍
EDGE_GAIN = 1.15  # 覆蓋率斜率銳化：把 AA 過渡帶壓到約 1/1.15 像素寬


# ---------------------------------------------------------------- 幾何


def geometry(n: int, hint: bool = True) -> dict:
    """算出尺寸 n 的盾牌幾何。

    `hint=True` 時把邊界四捨五入到整數像素格線上（欄位型別是 int，代表「必須落在
    格線上」）；`hint=False` 則保留連續值，交給高倍超取樣去解析。
    """
    if hint:
        apex: float = MARGIN
        bottom: float = n - MARGIN
    else:
        apex = n * MARGIN_RATIO
        bottom = n - apex
    h = bottom - apex  # 盾高吃滿畫布（扣掉安全邊）

    # 盾寬照原比例推；hint 時把左右側邊各自吸到整數格線，n 皆為偶數，鏡射即左右對稱
    w_ideal = h * F_ASPECT
    cx = n / 2
    if hint:
        x_l: float = round((n - w_ideal) / 2)
        # 半徑取整＝直徑為偶數，配上落在格點的圓心，圓的外接框剛好占滿整數像素。
        # 半徑照「理想盾寬」而非取整後的盾寬推算，免得 x_l 的捨入誤差被放大到環上。
        ring_r: float = max(1, round(w_ideal * F_RING_R))
        node_r: float = max(1, round(w_ideal * F_NODE_R))
        ring_cy: float = apex + round(h * F_RING_CY)
        shoulder: float = apex + round(h * F_SHOULDER)
        straight: float = apex + round(h * F_STRAIGHT)
    else:
        x_l = (n - w_ideal) / 2
        ring_r = w_ideal * F_RING_R
        node_r = w_ideal * F_NODE_R
        ring_cy = apex + h * F_RING_CY
        shoulder = apex + h * F_SHOULDER
        straight = apex + h * F_STRAIGHT
    x_r = n - x_l
    w = x_r - x_l

    return {
        "n": n,
        "hint": hint,
        "shield": {
            "x_l": x_l,
            "x_r": x_r,
            "w": w,
            "cx": cx,
            "apex": apex,
            "shoulder": shoulder,
            "straight": straight,
            "bottom": bottom,
            # 曲線控制點不需對齊，照比例縮放即可
            "c1y": apex + h * F_C1Y,
            "c2x": x_l + w * F_C2X,
            "c2y": apex + h * F_C2Y,
        },
        "ring": (cx, ring_cy, ring_r),
        "node": (cx, ring_cy, node_r),
    }


def bezier_steps(n: int) -> int:
    """盾底貝茲的攤平段數：小尺寸 64 段綽綽有餘，大尺寸要讓每段落在 2px 內。"""
    return max(64, n // 2)


def shield_polygon(g: dict) -> list[tuple[float, float]]:
    """盾形外框攤平成多邊形（底部兩段三次貝茲各切 steps 段）。"""
    s = g["shield"]
    n = g["n"]
    steps = bezier_steps(n)
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


def spans_circle(circle, y, hint: bool = True):
    """直徑大於 6px 用真圓（曲線的 AA 無法避免），hint 路徑下 6px 以下改走手繪像素圓盤。"""
    cx, cy, r = circle
    if hint and r <= 3:
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


class PolygonSpans:
    """多邊形的掃描線求交，邊先照像素列分桶。

    分桶純粹是為了速度：1024px 的盾形攤平後有上千條邊，每條 scanline 都全掃一遍會
    慢到不能用。求交的算式與收集後排序的結果都跟全掃版本一模一樣。
    """

    def __init__(self, pts: list[tuple[float, float]], n: int) -> None:
        self.rows: list[list[tuple[float, float, float, float]]] = [[] for _ in range(n + 1)]
        m = len(pts)
        for i in range(m):
            x0, y0 = pts[i]
            x1, y1 = pts[(i + 1) % m]
            if y0 == y1:  # 水平邊永遠不會被 (y0 <= y < y1) 取中
                continue
            lo, hi = (y0, y1) if y0 < y1 else (y1, y0)
            r0 = max(0, math.floor(lo))
            r1 = min(n, math.floor(hi))
            edge = (x0, y0, x1, y1)
            for r in range(r0, r1 + 1):
                self.rows[r].append(edge)

    def __call__(self, y: float):
        r = math.floor(y)
        if r < 0 or r >= len(self.rows):
            return ()
        xs = []
        for x0, y0, x1, y1 in self.rows[r]:
            if (y0 <= y < y1) or (y1 <= y < y0):
                xs.append(x0 + (y - y0) * (x1 - x0) / (y1 - y0))
        xs.sort()
        return tuple(zip(xs[0::2], xs[1::2]))


def coverage(n: int, span_fn, ssy: int = SSY, gain: float = EDGE_GAIN) -> list[list[float]]:
    """x 方向解析、y 方向超取樣的面積覆蓋率，再做一次邊緣斜率銳化。

    span 內部完全被蓋住的像素走「差分累加＋前綴和」，不必逐格加；只有兩端的部分覆
    蓋像素要算面積。這對結果沒有影響（內部像素的貢獻恆為 1/ssy，而 1/16、1/32 都是
    二進位可精確表示的數），純粹是讓 1024px 那層跑得完。
    """
    cov = []
    w = 1.0 / ssy
    for py in range(n):
        row = [0.0] * n
        delta = [0.0] * (n + 1)
        for i in range(ssy):
            y = py + (i + 0.5) / ssy
            for x0, x1 in span_fn(y):
                if x1 <= x0:
                    continue
                x0 = max(0.0, x0)
                x1 = min(float(n), x1)
                if x1 <= x0:
                    continue
                px0 = int(x0)
                px1 = min(n - 1, int(math.ceil(x1)) - 1)
                if px0 >= px1:
                    row[px0] += (x1 - x0) * w
                    continue
                row[px0] += (px0 + 1.0 - x0) * w
                row[px1] += (x1 - px1) * w
                if px1 > px0 + 1:
                    delta[px0 + 1] += w
                    delta[px1] -= w
        run = 0.0
        for px in range(n):
            run += delta[px]
            c = row[px] + run
            if 0.0 < c < 1.0:
                c = min(1.0, max(0.0, (c - 0.5) * gain + 0.5))
            row[px] = c
        cov.append(row)
    return cov


def grad_rgb(px: float, py: float, box: tuple[float, float, float, float]):
    """對角線漸層：t 取外接框單位座標的 (u+v)/2，等同 SVG 的 objectBoundingBox (0,0)→(1,1)。"""
    x0, y0, x1, y1 = box
    u = (px - x0) / (x1 - x0)
    v = (py - y0) / (y1 - y0)
    t = min(1.0, max(0.0, (u + v) / 2))
    a, b = GRAD
    return (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t)


def composite(n: int, layers) -> bytearray:
    """由下往上做 source-over，回傳 top-down 的 RGBA 位元組（背景全透明）。

    layers 的每一項是 (paint, cov)。paint 是實色 tuple，或 (px, py) -> rgb 的函式。
    """
    buf = bytearray(n * n * 4)
    for py in range(n):
        for px in range(n):
            # 以預乘色做 source-over，最後再還原
            pr = pg = pb = pa = 0.0
            for paint, cov in layers:
                c = cov[py][px]
                if c <= 0.0:
                    continue
                sr, sg, sb = paint if isinstance(paint, tuple) else paint(px + 0.5, py + 0.5)
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


def render_hinted(n: int) -> bytearray:
    g = geometry(n, hint=True)
    poly = PolygonSpans(shield_polygon(g), n)
    return composite(
        n,
        [
            (C_SHIELD, coverage(n, poly)),
            (C_HOLE, coverage(n, lambda y, c=g["ring"]: spans_circle(c, y))),
            (C_NODE, coverage(n, lambda y, c=g["node"]: spans_circle(c, y))),
        ],
    )


def render_smooth(n: int) -> bytearray:
    g = geometry(n, hint=False)
    s = g["shield"]
    poly = PolygonSpans(shield_polygon(g), n)
    _, ncy, nr = g["node"]
    # 盾身與節點各自把漸層套在自己的外接框上，等同 icon-final.svg 兩處都填 url(#teal)
    shield_box = (s["x_l"], s["apex"], s["x_r"], s["bottom"])
    node_box = (s["cx"] - nr, ncy - nr, s["cx"] + nr, ncy + nr)

    def cov(circle=None):
        span = poly if circle is None else (lambda y, c=circle: spans_circle(c, y, hint=False))
        return coverage(n, span, ssy=SSY_SMOOTH, gain=1.0)

    return composite(
        n,
        [
            (lambda x, y: grad_rgb(x, y, shield_box), cov()),
            (C_HOLE, cov(g["ring"])),
            (lambda x, y: grad_rgb(x, y, node_box), cov(g["node"])),
        ],
    )


def render(n: int) -> bytes:
    """尺寸 n 的 RGBA。SIZES 走像素對齊，其餘走平滑渲染。"""
    return bytes(render_hinted(n) if n in SIZES else render_smooth(n))


# ---------------------------------------------------------------- SVG


def fmt(v) -> str:
    return f"{v:g}"


def disc_shape(circle, fill: str, hint: bool) -> str:
    """把圓輸出成 SVG：hint 路徑下直徑 6px 以下走手繪像素圓盤，得畫成矩形集合才對得上光柵。"""
    cx, cy, r = circle
    if not hint or r > 3:
        return f'<circle cx="{fmt(cx)}" cy="{fmt(round(cy, 3))}" r="{fmt(round(r, 3))}" fill="{fill}"/>'
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


def shield_path(g: dict) -> str:
    s = g["shield"]
    n = g["n"]
    # 對齊過的欄位本來就是整數，round 到小數三位對它們是恆等；平滑幾何則靠這個收斂位數
    q = {k: round(v, 3) for k, v in s.items()}
    return (
        f"M{fmt(q['cx'])},{fmt(q['apex'])} L{fmt(q['x_r'])},{fmt(q['shoulder'])} "
        f"V{fmt(q['straight'])} "
        f"C{fmt(q['x_r'])},{fmt(q['c1y'])} {fmt(q['c2x'])},{fmt(q['c2y'])} "
        f"{fmt(q['cx'])},{fmt(q['bottom'])} "
        f"C{fmt(round(n - s['c2x'], 3))},{fmt(q['c2y'])} {fmt(q['x_l'])},{fmt(q['c1y'])} "
        f"{fmt(q['x_l'])},{fmt(q['straight'])} V{fmt(q['shoulder'])} Z"
    )


def to_svg(n: int, hint: bool, gradient: bool, note: str) -> str:
    g = geometry(n, hint=hint)
    if gradient:
        (r0, g0, b0), (r1, g1, b1) = GRAD
        defs = (
            "  <defs>\n"
            '    <linearGradient id="teal" x1="0" y1="0" x2="1" y2="1">\n'
            f'      <stop offset="0" stop-color="#{r0:02x}{g0:02x}{b0:02x}"/>\n'
            f'      <stop offset="1" stop-color="#{r1:02x}{g1:02x}{b1:02x}"/>\n'
            "    </linearGradient>\n"
            "  </defs>\n"
        )
        shield_fill = node_fill = "url(#teal)"
    else:
        defs = ""
        shield_fill = "#%02x%02x%02x" % C_SHIELD
        node_fill = "#%02x%02x%02x" % C_NODE
    hole_fill = "#%02x%02x%02x" % C_HOLE
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}">\n'
        f"  <!-- {note}\n"
        f"       由 assets/gen-tray-icons.py 產生，請勿手改。 -->\n"
        f"{defs}"
        f'  <path d="{shield_path(g)}" fill="{shield_fill}"/>\n'
        f"  {disc_shape(g['ring'], hole_fill, hint)}\n"
        f"  {disc_shape(g['node'], node_fill, hint)}\n"
        f"</svg>\n"
    )


# ---------------------------------------------------------------- 容器


def png_bytes(n: int, rgba: bytes) -> bytes:
    raw = b"".join(b"\x00" + bytes(rgba[y * n * 4 : (y + 1) * n * 4]) for y in range(n))

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    return (
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


def ico_bytes(sizes, rasters: dict[int, bytes]) -> bytes:
    """整顆 ICO 重組。全部圖層都用 32bpp DIB，含 256px（Windows 也吃 PNG，但沒必要換）。"""
    off = 6 + 16 * len(sizes)
    entries = bytearray()
    blobs = bytearray()
    for n in sizes:
        blob = dib_layer(n, rasters[n])
        dim = 0 if n >= 256 else n  # ICO 用 0 表示 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), off)
        blobs += blob
        off += len(blob)
    return b"\x00\x00\x01\x00" + struct.pack("<H", len(sizes)) + bytes(entries) + bytes(blobs)


def icns_bytes(pngs: dict[int, bytes]) -> bytes:
    body = bytearray()
    for tag, n in ICNS_ENTRIES:
        data = pngs[n]
        body += tag + struct.pack(">I", 8 + len(data)) + data
    return b"icns" + struct.pack(">I", 8 + len(body)) + bytes(body)


# ---------------------------------------------------------------- 主流程


def main() -> None:
    ap = argparse.ArgumentParser(description="Traytunnel 圖示工廠")
    ap.add_argument("--png-out", type=Path, help="另外把各尺寸存成 PNG（目檢用）")
    args = ap.parse_args()

    wanted = sorted({*SIZES, *ICO_SIZES, *PNG_TARGETS.values(), *(n for _, n in ICNS_ENTRIES)})
    rasters: dict[int, bytes] = {}
    for n in wanted:
        rasters[n] = render(n)
        g = geometry(n, hint=n in SIZES)
        s = g["shield"]
        aa = sum(1 for i in range(n * n) if 0 < rasters[n][i * 4 + 3] < 255)
        kind = "hint" if n in SIZES else "平滑"
        print(
            f"  {n:>4}px [{kind}] 盾 x {fmt(round(s['x_l'], 2))}..{fmt(round(s['x_r'], 2))}"
            f" y {fmt(round(s['apex'], 2))}..{fmt(round(s['bottom'], 2))}"
            f" 環 r={fmt(round(g['ring'][2], 2))} 節點 r={fmt(round(g['node'][2], 2))} AA {aa}"
        )

    pngs = {n: png_bytes(n, rasters[n]) for n in wanted}

    icons = REPO / "src-tauri" / "icons"
    for name, n in PNG_TARGETS.items():
        (icons / name).write_bytes(pngs[n])
    print(f"  src-tauri/icons：寫了 {len(PNG_TARGETS)} 個 PNG")

    (icons / "icon.icns").write_bytes(icns_bytes(pngs))
    print(f"  icon.icns：{', '.join(f'{t.decode()}={n}' for t, n in ICNS_ENTRIES)}")

    blob = ico_bytes(ICO_SIZES, rasters)
    for rel in ICO_TARGETS:
        (REPO / rel).write_bytes(blob)
        print(f"  {rel}：{len(ICO_SIZES)} 層 {list(ICO_SIZES)}，{len(blob)} bytes")

    for n in SIZES:
        (REPO / "assets" / f"icon-tray-{n}.svg").write_text(
            to_svg(n, True, False, f"{n}px 系統匣圖層：像素對齊版，背景透明、垂直邊落在整數像素格線上。"),
            encoding="utf-8",
        )
    (REPO / "assets" / "icon-final.svg").write_text(
        to_svg(256, False, True, "256 完整版（無底板）：大尺寸光柵的向量對照，含對角 teal 漸層。"),
        encoding="utf-8",
    )
    (REPO / "public" / "icon.svg").write_text(
        to_svg(
            256,
            False,
            False,
            "標題列（16 CSS px）與 favicon 用：幾何與工廠同源，"
            "配色跟著系統匣那幾層走實色——這裡實際只渲染 16px，漸層在這個尺度只會糊成一坨濁色。",
        ),
        encoding="utf-8",
    )
    print("  assets/icon-tray-*.svg、assets/icon-final.svg、public/icon.svg 已更新")

    if args.png_out:
        args.png_out.mkdir(parents=True, exist_ok=True)
        for n in wanted:
            (args.png_out / f"icon-{n}.png").write_bytes(pngs[n])
        print(f"  預覽 PNG：{args.png_out}")


if __name__ == "__main__":
    main()

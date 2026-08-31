#!/usr/bin/env python3
"""macOS 系統匣 template 圖示產生器：純黑＋透明剪影，供 `icon_as_template(true)` 用。

為什麼要另外一支腳本
--------------------
`assets/gen-tray-icons.py` 產出的是**彩色**圖層（盾身 teal、通道環近黑、中央節點
亮 teal），Windows 系統匣直接吃它的顏色。macOS 的 template image 機制完全不看
RGB——系統只用 alpha 通道當剪影，依明暗模式（與選取狀態）自動套色——硬把彩色 PNG
設成 template 只會依它「剛好長什麼顏色」畫出走樣的黑白剪影，通道環那圈近黑色會
整片糊成不透明。

所以這裡不是把既有 PNG 轉黑白，而是重新合成一次：沿用 `gen-tray-icons.py` 的盾形
幾何（`geometry`／`shield_polygon`／`coverage`／`spans_circle`，直接 import 那支
腳本取用，不重複一份），但把「通道環」與「中央節點」的意義換成**真的 alpha 挖
洞**——盾形先填滿黑色不透明，環的範圍整個挖成透明，節點再蓋回一顆黑色實心圓，
結果是一個「盾牌剪影中間有個環形透明窗、窗裡一顆黑點」的圖形，跟原圖的視覺語意
（盾＋通道＋節點）對得上，也是 template image 該有的形狀：只靠 alpha 就看得出
輪廓。

尺寸
----
Apple 選單列圖示的建議尺寸是 22×22pt；Retina（2x）算下來是 44×44px。tray-icon
這顆底層 crate 會把顯示高度另外定死在 18pt（跟這裡給的像素尺寸無關，見
`platform/macos/sys.rs::small_icon_size` 的說明），所以這裡的 44px 純粹是給
Retina 螢幕留解析度，不是最終顯示大小。

用法：`python assets/gen-tray-template.py [--preview-out DIR]`
產物：`src-tauri/icons/tray-template.png`（44×44，RGBA，純黑＋透明）
`--preview-out DIR` 額外存一份合成在中灰背景上的版本，方便肉眼檢查透明區域
（一般圖檢視器對「全透明」的 PNG 不容易看出形狀）。
"""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ASSETS = Path(__file__).resolve().parent

# 檔名含連字號，`import` 語法用不了，改用 importlib 直接照路徑載入既有那支腳本，
# 借它的幾何與 PNG 編碼函式，不重複實作一份
_spec = importlib.util.spec_from_file_location("gen_tray_icons", ASSETS / "gen-tray-icons.py")
factory = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(factory)

N = 44  # 22pt @ 2x（Retina），見本檔開頭說明


def render_template(n: int) -> bytes:
    """合成 template 用的 RGBA：黑色盾形剪影，通道環挖成透明窗，節點蓋回黑點。"""
    g = factory.geometry(n, hint=False)
    poly = factory.PolygonSpans(factory.shield_polygon(g), n)

    def cov(span_fn):
        return factory.coverage(n, span_fn, ssy=factory.SSY_SMOOTH, gain=1.0)

    shield = cov(poly)
    hole = cov(lambda y, c=g["ring"]: factory.spans_circle(c, y, hint=False))
    node = cov(lambda y, c=g["node"]: factory.spans_circle(c, y, hint=False))

    buf = bytearray(n * n * 4)
    for py in range(n):
        for px in range(n):
            a = shield[py][px] * (1.0 - hole[py][px])
            a = max(a, node[py][px])
            o = (py * n + px) * 4
            # RGB 對 template image 沒有意義（系統只看 alpha），統一填黑，
            # 讓沒套用 isTemplate 的檢視器（例如直接開檔預覽）看到的也是一個黑色剪影
            buf[o] = buf[o + 1] = buf[o + 2] = 0
            buf[o + 3] = min(255, round(a * 255))
    return bytes(buf)


def composite_on_gray(rgba: bytes, n: int, bg=(0x80, 0x80, 0x80)) -> bytes:
    """把 template 疊在中灰背景上，方便在一般圖檢視器裡肉眼檢查透明窗的形狀。"""
    out = bytearray(n * n * 3)
    for i in range(n * n):
        r, g, b, a = rgba[i * 4 : i * 4 + 4]
        af = a / 255
        out[i * 3 + 0] = round(r * af + bg[0] * (1 - af))
        out[i * 3 + 1] = round(g * af + bg[1] * (1 - af))
        out[i * 3 + 2] = round(b * af + bg[2] * (1 - af))
    return bytes(out)


def main() -> None:
    ap = argparse.ArgumentParser(description="macOS 系統匣 template 圖示產生器")
    ap.add_argument("--preview-out", type=Path, help="另存一份灰底合成版，方便肉眼檢查（目檢用）")
    ap.add_argument("--size", type=int, default=N, help=f"輸出像素尺寸，預設 {N}（22pt @2x）")
    args = ap.parse_args()

    rgba = render_template(args.size)
    out = REPO / "src-tauri" / "icons" / "tray-template.png"
    out.write_bytes(factory.png_bytes(args.size, rgba))
    aa = sum(1 for i in range(args.size * args.size) if 0 < rgba[i * 4 + 3] < 255)
    opaque = sum(1 for i in range(args.size * args.size) if rgba[i * 4 + 3] >= 250)
    transparent = sum(1 for i in range(args.size * args.size) if rgba[i * 4 + 3] <= 5)
    print(f"  {out.relative_to(REPO)}：{args.size}x{args.size}，AA {aa}px，不透明 {opaque}px，全透明 {transparent}px")

    if args.preview_out:
        args.preview_out.mkdir(parents=True, exist_ok=True)
        preview = composite_on_gray(rgba, args.size)
        preview_path = args.preview_out / "tray-template-preview.png"
        preview_path.write_bytes(factory.png_bytes(args.size, preview, color_type=2))
        print(f"  灰底預覽：{preview_path}")


if __name__ == "__main__":
    main()

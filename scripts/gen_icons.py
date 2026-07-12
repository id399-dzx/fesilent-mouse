#!/usr/bin/env python3
"""MouseShare 图标生成器（纯标准库，无第三方依赖）。

用 SDF（有符号距离场）+ 1px 抗锯齿离线绘制图标，避免依赖 svg/PIL：
  - app 图标：渐变 squircle 背景 + 两块屏幕 + 跨中缝的白色鼠标指针
    （隐喻：一个光标在两台机器间共享穿越）。
  - tray 图标：单色鼠标指针剪影（mac 设为模板图标自适应深浅色，Win 显白）。

用法：python3 scripts/gen_icons.py
输出：crates/app/icons/{icon.png,icon-32.png,tray.png}
"""
import math
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "app", "icons")

# 经典鼠标指针多边形（相对单位，尖端在原点，y 向下）。
ARROW = [
    (0.00, 0.00),
    (0.00, 0.72),
    (0.19, 0.55),
    (0.29, 0.80),
    (0.40, 0.76),
    (0.30, 0.51),
    (0.52, 0.51),
]


def sdf_round_box(px, py, cx, cy, hw, hh, r):
    dx = abs(px - cx) - hw + r
    dy = abs(py - cy) - hh + r
    ax, ay = max(dx, 0.0), max(dy, 0.0)
    return math.hypot(ax, ay) + min(max(dx, dy), 0.0) - r


def sdf_polygon(px, py, pts):
    """点到多边形有符号距离（内负外正），iq 经典算法。"""
    n = len(pts)
    d = (px - pts[0][0]) ** 2 + (py - pts[0][1]) ** 2
    sign = 1.0
    for i in range(n):
        j = (i - 1) % n
        ex, ey = pts[j][0] - pts[i][0], pts[j][1] - pts[i][1]
        wx, wy = px - pts[i][0], py - pts[i][1]
        t = max(0.0, min(1.0, (wx * ex + wy * ey) / (ex * ex + ey * ey)))
        bx, by = wx - ex * t, wy - ey * t
        d = min(d, bx * bx + by * by)
        c1 = py >= pts[i][1]
        c2 = py < pts[j][1]
        c3 = wx * ey > wy * ex
        if (c1 and c2 and c3) or (not c1 and not c2 and not c3):
            sign = -sign
    return sign * math.sqrt(d)


def cover(sdf):
    """1px 抗锯齿覆盖率：sdf<=-0.5→1，>=0.5→0。"""
    return max(0.0, min(1.0, 0.5 - sdf))


def over(dst, src):
    """src 叠加到 dst（均为 (r,g,b,a)，a∈[0,1]）。"""
    sa = src[3]
    if sa <= 0:
        return dst
    da = dst[3]
    oa = sa + da * (1 - sa)
    if oa <= 0:
        return (0.0, 0.0, 0.0, 0.0)
    out = [(src[i] * sa + dst[i] * da * (1 - sa)) / oa for i in range(3)]
    return (out[0], out[1], out[2], oa)


def lerp(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def render(size, mode):
    s = float(size)
    px = bytearray(size * size * 4)

    # 渐变背景色
    c1 = (74, 144, 255)   # 蓝
    c2 = (142, 84, 255)   # 紫
    scr_fill = (255, 255, 255)
    cur = (255, 255, 255)
    outline = (28, 36, 64)

    # 几何（相对 size）
    rad = 0.215 * s                       # squircle 圆角
    seam = 0.5 * s                         # 中缝
    scr_hw, scr_hh, scr_r = 0.165 * s, 0.135 * s, 0.045 * s
    scr_cy = 0.5 * s
    scr_lx, scr_rx = 0.315 * s, 0.685 * s

    # 指针变换：尖端落点 + 缩放
    cur_scale = 0.46 * s
    cur_tipx, cur_tipy = 0.40 * s, 0.30 * s
    arrow = [(cur_tipx + ax * cur_scale, cur_tipy + ay * cur_scale) for ax, ay in ARROW]
    sh_off = 0.012 * s  # 阴影偏移
    # 指针包围盒（含阴影偏移 + 描边余量），盒外跳过多边形 SDF 提速
    axs = [p[0] for p in arrow]
    ays = [p[1] for p in arrow]
    m = 0.02 * s + sh_off
    abx0, abx1 = min(axs) - m, max(axs) + m
    aby0, aby1 = min(ays) - m, max(ays) + m

    for y in range(size):
        for x in range(size):
            fx, fy = x + 0.5, y + 0.5
            col = (0.0, 0.0, 0.0, 0.0)

            if mode == "app":
                # 背景渐变 squircle
                bg = sdf_round_box(fx, fy, s / 2, s / 2, s / 2, s / 2, rad)
                a = cover(bg)
                if a > 0:
                    t = (fx + fy) / (2 * s)
                    rgb = lerp(c1, c2, t)
                    col = over(col, (rgb[0], rgb[1], rgb[2], a))
                # 两块屏幕（半透明白填充 + 描边）
                for cx in (scr_lx, scr_rx):
                    sb = sdf_round_box(fx, fy, cx, scr_cy, scr_hw, scr_hh, scr_r)
                    col = over(col, (scr_fill[0], scr_fill[1], scr_fill[2], cover(sb) * 0.18))
                    edge = abs(sb) - 0.006 * s
                    col = over(col, (255, 255, 255, cover(edge) * 0.55))
                if abx0 <= fx <= abx1 and aby0 <= fy <= aby1:
                    # 指针阴影
                    shd = sdf_polygon(fx - sh_off, fy - sh_off, arrow)
                    col = over(col, (0, 0, 0, cover(shd) * 0.35))
                    # 指针白填充
                    cd = sdf_polygon(fx, fy, arrow)
                    col = over(col, (cur[0], cur[1], cur[2], cover(cd)))
                    # 指针深色描边
                    ed = abs(cd) - 0.009 * s
                    col = over(col, (outline[0], outline[1], outline[2], cover(ed)))
            else:  # tray：纯指针剪影（白），alpha 即形状
                ta = [(a * 0.92 * s, b * 0.92 * s) for a, b in ARROW]
                # 居中：用包围盒重定位
                xs = [p[0] for p in ta]
                ys = [p[1] for p in ta]
                ox = (s - (max(xs) - min(xs))) / 2 - min(xs)
                oy = (s - (max(ys) - min(ys))) / 2 - min(ys)
                tp = [(p[0] + ox, p[1] + oy) for p in ta]
                cd = sdf_polygon(fx, fy, tp)
                col = over(col, (255, 255, 255, cover(cd)))

            i = (y * size + x) * 4
            px[i] = int(col[0] + 0.5)
            px[i + 1] = int(col[1] + 0.5)
            px[i + 2] = int(col[2] + 0.5)
            px[i + 3] = int(col[3] * 255 + 0.5)
    return bytes(px)


def write_png(path, size, rgba):
    def chunk(tag, data):
        return (struct.pack(">I", len(data)) + tag + data
                + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

    raw = bytearray()
    stride = size * 4
    for y in range(size):
        raw.append(0)  # filter: none
        raw += rgba[y * stride:(y + 1) * stride]
    out = b"\x89PNG\r\n\x1a\n"
    out += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    out += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    out += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(out)
    print(f"✓ {path} ({size}x{size}, {len(out)} bytes)")


def main():
    os.makedirs(OUT, exist_ok=True)
    write_png(os.path.join(OUT, "icon.png"), 1024, render(1024, "app"))
    write_png(os.path.join(OUT, "icon-32.png"), 32, render(32, "app"))
    write_png(os.path.join(OUT, "tray.png"), 44, render(44, "tray"))


if __name__ == "__main__":
    main()

"""生成程序图标 assets/app.ico（多尺寸 32 位 BGRA）。

图标内容：深色圆角方块 + 青色「端口插座」圆环，小尺寸下依然可辨认。
修改图标后重新运行：python assets/make_icon.py
"""

import math
import struct
import zlib
from pathlib import Path

# 生成 16/32/48 三种尺寸；256 尺寸用 BMP 存放会占用 256 KB，得不偿失，故不提供
SIZES = (16, 32, 48)
# 另出一份 PNG 供窗口/任务栏图标使用：eframe 只认 PNG，Windows 只认 ICO
PNG_SIZE = 64
SUPERSAMPLE = 4  # 先按 4 倍分辨率绘制再降采样，得到抗锯齿边缘

BACKGROUND = (0x1E, 0x29, 0x3B)  # 深板岩蓝
RING = (0x38, 0xBD, 0xF8)  # 天蓝
CORE = (0xF8, 0xFA, 0xFC)  # 近白


def rounded_square_coverage(x: float, y: float, size: float, radius: float) -> bool:
    """判断点 (x, y) 是否落在边长为 size、圆角半径为 radius 的圆角方块内。"""
    cx = min(max(x, radius), size - radius)
    cy = min(max(y, radius), size - radius)
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius**2


def render(size: int) -> list[list[tuple[int, int, int, int]]]:
    """按 SUPERSAMPLE 倍分辨率绘制再降采样，返回 RGBA 像素矩阵。"""
    big = size * SUPERSAMPLE
    center = big / 2.0
    radius = big * 0.28  # 圆角半径
    ring_outer = big * 0.32
    ring_inner = big * 0.21
    core_radius = big * 0.11

    # 高分辨率下逐点判定颜色，再对 SUPERSAMPLE×SUPERSAMPLE 个采样点求平均
    def sample(px: float, py: float) -> tuple[int, int, int, int]:
        if not rounded_square_coverage(px, py, big, radius):
            return (0, 0, 0, 0)
        distance = math.hypot(px - center, py - center)
        if distance <= core_radius:
            return (*CORE, 255)
        if ring_inner <= distance <= ring_outer:
            return (*RING, 255)
        return (*BACKGROUND, 255)

    pixels = []
    for y in range(size):
        row = []
        for x in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    px = x * SUPERSAMPLE + sx + 0.5
                    py = y * SUPERSAMPLE + sy + 0.5
                    value = sample(px, py)
                    for channel in range(4):
                        acc[channel] += value[channel]
            total = SUPERSAMPLE * SUPERSAMPLE
            row.append(tuple(channel // total for channel in acc))
        pixels.append(row)
    return pixels


def encode_dib(pixels: list[list[tuple[int, int, int, int]]]) -> bytes:
    """把 RGBA 像素编码成 ICO 内嵌的 BITMAPINFOHEADER + BGRA 数据 + AND 掩码。"""
    size = len(pixels)
    header = struct.pack(
        "<IiiHHIIiiII",
        40,  # biSize
        size,  # biWidth
        size * 2,  # biHeight：ICO 要求为实际高度的两倍（含掩码）
        1,  # biPlanes
        32,  # biBitCount
        0,  # biCompression = BI_RGB
        0,  # biSizeImage
        0,
        0,
        0,
        0,
    )
    body = bytearray()
    # BMP 的像素数据自下而上存放
    for row in reversed(pixels):
        for red, green, blue, alpha in row:
            body += bytes((blue, green, red, alpha))
    # 32 位图标仍必须带 AND 掩码，全 0 表示完全由 alpha 通道决定透明度
    mask_row = (size + 31) // 32 * 4
    body += bytes(mask_row * size)
    return header + bytes(body)


def encode_png(pixels: list[list[tuple[int, int, int, int]]]) -> bytes:
    """把 RGBA 像素编码成 PNG（只用标准库，避免为一次性的图标生成引入 Pillow 依赖）。"""
    size = len(pixels)
    raw = bytearray()
    for row in pixels:
        raw.append(0)  # 每行的过滤器类型：0 = None
        for red, green, blue, alpha in row:
            raw += bytes((red, green, blue, alpha))

    def chunk(tag: bytes, data: bytes) -> bytes:
        payload = tag + data
        return struct.pack(">I", len(data)) + payload + struct.pack(">I", zlib.crc32(payload) & 0xFFFFFFFF)

    # 位深 8、颜色类型 6（RGBA）、无隔行
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def main() -> None:
    images = [(size, encode_dib(render(size))) for size in SIZES]

    directory = struct.pack("<HHH", 0, 1, len(images))
    offset = len(directory) + 16 * len(images)
    entries = bytearray()
    for size, data in images:
        entries += struct.pack(
            "<BBBBHHII",
            size,  # 宽（256 需写 0，本文件不含该尺寸）
            size,  # 高
            0,  # 调色板颜色数
            0,  # 保留
            1,  # 色彩平面数
            32,  # 位深
            len(data),
            offset,
        )
        offset += len(data)

    target = Path(__file__).with_name("app.ico")
    target.write_bytes(directory + bytes(entries) + b"".join(data for _, data in images))
    print(f"已生成 {target}（{target.stat().st_size} 字节，尺寸 {', '.join(str(s) for s in SIZES)}）")

    png = Path(__file__).with_name("icon.png")
    png.write_bytes(encode_png(render(PNG_SIZE)))
    print(f"已生成 {png}（{png.stat().st_size} 字节，尺寸 {PNG_SIZE}）")


if __name__ == "__main__":
    main()

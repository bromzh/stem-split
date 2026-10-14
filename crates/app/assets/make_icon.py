"""Draws the application icon and writes it in the two forms the build needs.

    python make_icon.py

Outputs, next to this script:

    icon.ico      embedded in the executable; what the file manager shows
    icon-64.rgba  64x64 raw RGBA; the icon of the window and the taskbar

The picture is four horizontal bars of different lengths on a dark rounded
square: one track split into four stems. It is drawn here, in plain Python,
so that changing it needs no image editor and no extra packages.
"""
import pathlib
import struct
import zlib

HERE = pathlib.Path(__file__).parent

BACKGROUND = (27, 31, 42)
# Drums, bass, other, vocals: top to bottom.
BARS = [
    ((242, 95, 92), 0.78),
    ((255, 205, 86), 0.52),
    ((112, 193, 179), 0.92),
    ((80, 166, 255), 0.64),
]
# Samples per pixel along each axis; smooths the rounded edges.
SUPERSAMPLING = 4


def inside_rounded(x, y, left, top, right, bottom, radius):
    """Whether the point lies in a rectangle with rounded corners."""
    if not (left <= x <= right and top <= y <= bottom):
        return False
    nearest_x = min(max(x, left + radius), right - radius)
    nearest_y = min(max(y, top + radius), bottom - radius)
    return (x - nearest_x) ** 2 + (y - nearest_y) ** 2 <= radius ** 2


def colour_at(x, y):
    """Colour of a point in the unit square, or None outside the icon."""
    if not inside_rounded(x, y, 0.03, 0.03, 0.97, 0.97, 0.2):
        return None
    left, right = 0.2, 0.8
    top, height, gap = 0.215, 0.105, 0.05
    for index, (colour, length) in enumerate(BARS):
        bar_top = top + index * (height + gap)
        bar_right = left + (right - left) * length
        if inside_rounded(x, y, left, bar_top, bar_right, bar_top + height, height / 2):
            return colour
    return BACKGROUND


def render(size):
    """The icon as `size` x `size` RGBA bytes, top row first."""
    pixels = bytearray()
    samples = SUPERSAMPLING * SUPERSAMPLING
    for row in range(size):
        for column in range(size):
            red = green = blue = covered = 0
            for sub_y in range(SUPERSAMPLING):
                for sub_x in range(SUPERSAMPLING):
                    colour = colour_at(
                        (column + (sub_x + 0.5) / SUPERSAMPLING) / size,
                        (row + (sub_y + 0.5) / SUPERSAMPLING) / size,
                    )
                    if colour is not None:
                        red += colour[0]
                        green += colour[1]
                        blue += colour[2]
                        covered += 1
            if covered:
                pixels += bytes((
                    round(red / covered),
                    round(green / covered),
                    round(blue / covered),
                    round(255 * covered / samples),
                ))
            else:
                pixels += bytes(4)
    return bytes(pixels)


def png(size, rgba):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    rows = b"".join(b"\x00" + rgba[row * size * 4:(row + 1) * size * 4] for row in range(size))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def bitmap(size, rgba):
    """An icon image in the classic format: a bottom-up BGRA bitmap and a mask."""
    header = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    rows = []
    for row in reversed(range(size)):
        line = rgba[row * size * 4:(row + 1) * size * 4]
        rows.append(b"".join(
            bytes((line[i + 2], line[i + 1], line[i], line[i + 3])) for i in range(0, len(line), 4)
        ))
    # The mask is unused with 32-bit colour but must be present: one bit per
    # pixel, each row padded to four bytes.
    mask = bytes(((size + 31) // 32) * 4) * size
    return header + b"".join(rows) + mask


def ico(images):
    """An .ico file from (size, encoded image) pairs."""
    directory = struct.pack("<HHH", 0, 1, len(images))
    offset = len(directory) + 16 * len(images)
    entries = b""
    for size, data in images:
        # A size of 256 is written as 0.
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return directory + entries + b"".join(data for _, data in images)


def main():
    rendered = {size: render(size) for size in (16, 24, 32, 48, 64, 256)}
    # Small sizes as bitmaps, which every consumer reads; the large one as
    # PNG, the usual way to keep the file small.
    images = [(size, bitmap(size, rendered[size])) for size in (16, 24, 32, 48, 64)]
    images.append((256, png(256, rendered[256])))
    (HERE / "icon.ico").write_bytes(ico(images))
    (HERE / "icon-64.rgba").write_bytes(rendered[64])
    (HERE / "icon-preview.png").write_bytes(png(256, rendered[256]))
    print("written: icon.ico, icon-64.rgba, icon-preview.png")


if __name__ == "__main__":
    main()

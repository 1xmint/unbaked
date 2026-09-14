"""Upscales 8-bit RGBA PNGs and stacks them into one contact sheet, over a checkerboard.

    python tests/conformance/tools/sheet.py OUT.png SCALE IN.png [IN.png ...]
"""
import struct, sys, zlib

def read_png(path):
    data = open(path, "rb").read()
    pos, idat = 8, b""
    while pos < len(data):
        (n,) = struct.unpack(">I", data[pos:pos + 4]); kind = data[pos + 4:pos + 8]; body = data[pos + 8:pos + 8 + n]
        if kind == b"IHDR": w, h, depth, ctype = struct.unpack(">IIBB", body[:10])
        if kind == b"IDAT": idat += body
        pos += 12 + n
    assert depth == 8 and ctype == 6, path
    raw, bpp, rows, prev = zlib.decompress(idat), 4, [], bytearray(w * 4)
    for y in range(h):
        f = raw[y * (w * 4 + 1)]; line = bytearray(raw[y * (w * 4 + 1) + 1:(y + 1) * (w * 4 + 1)])
        for i in range(len(line)):
            a = line[i - bpp] if i >= bpp else 0; b = prev[i]; c = prev[i - bpp] if i >= bpp else 0
            if f == 1: line[i] = (line[i] + a) & 255
            elif f == 2: line[i] = (line[i] + b) & 255
            elif f == 3: line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c; pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line); prev = line
    return w, h, rows

out, scale, gap = sys.argv[1], int(sys.argv[2]), 6
images = [read_png(p) for p in sys.argv[3:]]
W = max(w for w, _, _ in images) * scale; H = sum(h * scale + gap for _, h, _ in images)
canvas = []
for w, h, rows in images:
    for y in range(h * scale):
        line = bytearray()
        for x in range(W // scale * scale):
            cx, cy = x // scale, y // scale
            check = 200 if ((x // 8) + (y // 8)) % 2 else 150
            if cx < w:
                r, g, b, a = rows[cy][cx * 4:cx * 4 + 4]
                line += bytes(round((v * a + check * (255 - a)) / 255) for v in (r, g, b))
            else:
                line += bytes((60, 60, 60))
        canvas.append(line)
    canvas += [bytearray((255, 0, 255)) * (W // scale * scale)] * gap
width = W // scale * scale
raw = b"".join(b"\0" + bytes(l) for l in canvas)
def chunk(k, d): return struct.pack(">I", len(d)) + k + d + struct.pack(">I", zlib.crc32(k + d))
open(out, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, len(canvas), 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))

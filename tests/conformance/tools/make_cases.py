"""Builds the tests/conformance/<case>/package folders.

    python tests/conformance/tools/make_cases.py
        Rewrites every package. The M4A source is the committed
        audio-mix/package/assets/voice.m4a, so a run reproduces the tree.

    python tests/conformance/tools/make_cases.py --voice-recipe DIR
        Writes the recipe folder the M4A source was rendered from. To replace it:
        `unbaked render DIR -o voice.m4a`, then
        `make_cases.py --voice voice.m4a`.

    python tests/conformance/tools/make_cases.py --bench DIR
        Writes full-size benchmark recipe folders into DIR instead: a 1024x1024
        image with text and a blur, a 1920x1080 four-layer image, a 60 s stereo
        mix of three clips, and a 10 s 1080p video. Render them with
        `--fonts tests/fonts`.
"""
import argparse, array, hashlib, json, math, os, shutil, struct, zlib

repo = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))
root = os.path.join(repo, "tests", "conformance")


def png(width, height, pixel):
    raw = b"".join(b"\0" + b"".join(bytes(pixel(x, y)) for x in range(width)) for y in range(height))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def wav(rate, channels, frames):
    data = b"".join(struct.pack("<h", max(-32768, min(32767, round(s * 32767)))) for f in frames for s in f)
    return (b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVE"
            + b"fmt " + struct.pack("<IHHIIHH", 16, 1, channels, rate, rate * channels * 2, channels * 2, 16)
            + b"data" + struct.pack("<I", len(data)) + data)


def webp_lossless(width, height, pixel):
    """A lossless WebP (VP8L) with no transforms and every channel coded in 8 bits."""
    bits = []

    def put(value, n):
        bits.extend((value >> i) & 1 for i in range(n))

    def prefix_code(alphabet, used):
        # A normal prefix code: the code length code gives symbols 0 and 8 one bit
        # each, then `used` symbols get length 8 and the rest 0.
        put(0, 1)
        order = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8]
        put(len(order) - 4, 4)
        for symbol in order:
            put(1 if symbol in (0, 8) else 0, 3)
        put(0, 1)  # lengths for the whole alphabet
        for s in range(alphabet):
            put(1 if s < used else 0, 1)

    def symbol(value):
        # Canonical codes read most significant bit first.
        for i in range(7, -1, -1):
            put((value >> i) & 1, 1)

    put(0x2F, 8)
    put(width - 1, 14)
    put(height - 1, 14)
    put(1, 1)  # alpha is used
    put(0, 3)  # version
    put(0, 1)  # no transform
    put(0, 1)  # no colour cache
    put(0, 1)  # no meta prefix codes
    prefix_code(256 + 24, 256)  # green and lengths
    for _ in range(3):  # red, blue, alpha
        prefix_code(256, 256)
    put(1, 1)  # distance: a simple code with one symbol
    put(0, 1)
    put(0, 1)
    put(0, 1)
    for y in range(height):
        for x in range(width):
            r, g, b, a = pixel(x, y)
            for v in (g, r, b, a):
                symbol(v)
    data = bytes(sum(bit << i for i, bit in enumerate(bits[k:k + 8])) for k in range(0, len(bits), 8))
    chunk = b"VP8L" + struct.pack("<I", len(data)) + data + (b"\0" if len(data) % 2 else b"")
    return b"RIFF" + struct.pack("<I", 4 + len(chunk)) + b"WEBP" + chunk


def tone(freq, rate, ms, amp):
    return [amp * math.sin(2 * math.pi * freq * k / rate) for k in range(rate * ms // 1000)]


def gradient(x, y):
    return (x * 255 // 47, y * 255 // 31, 128, 255)


def badge(x, y):
    d = math.hypot(x - 3.5, y - 3.5)
    alpha = 255 if d < 2.6 else 128 if d < 3.8 else 0
    return (x * 32, 255 - y * 32, (x + y) * 16, alpha)


ASSETS = {
    "gradient.png": lambda: png(48, 32, gradient),
    "badge.png": lambda: png(8, 8, badge),
    # Partly transparent, so the alpha channel's code is exercised too.
    "swatch.webp": lambda: webp_lossless(12, 10, lambda x, y: (x * 21, y * 25, 255 - x * 10, 255 - (x + y) * 12)),
    "tone-44k-mono.wav": lambda: wav(44100, 1, [(s,) for s in tone(440, 44100, 250, 0.4)]),
    "chord-48k-stereo.wav": lambda: wav(48000, 2, list(zip(tone(330, 48000, 200, 0.3), tone(550, 48000, 200, 0.3)))),
}

def write_voice_recipe(folder):
    os.makedirs(os.path.join(folder, "assets"), exist_ok=True)
    with open(os.path.join(folder, "assets", "voice.wav"), "wb") as f:
        # A 660 Hz tone that swells, so edit-list timing errors show.
        f.write(wav(44100, 1, [(s * k / 8820,) for k, s in enumerate(tone(660, 44100, 200, 0.5))]))
    with open(os.path.join(folder, "recipe.json"), "w", newline="\n") as f:
        json.dump({"unbaked": 0, "output": {"kind": "audio", "duration_ms": 200, "sample_rate": 44100, "channels": 1},
                   "assets": {"voice": {"path": "assets/voice.wav"}}, "audio": [{"id": "voice", "asset": "voice"}]}, f)


parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--voice-recipe", metavar="DIR", help="write the M4A source's recipe folder and stop")
parser.add_argument("--voice", metavar="M4A", help="M4A source to use instead of the committed one")
parser.add_argument("--bench", metavar="DIR", help="write benchmark recipe folders into DIR and stop")
args = parser.parse_args()

if args.voice_recipe:
    write_voice_recipe(args.voice_recipe)
    raise SystemExit(0)

# Read before any package folder is removed, since the default lives inside one.
voice_m4a = open(args.voice or os.path.join(root, "audio-mix", "package", "assets", "voice.m4a"), "rb").read()
lato = hashlib.sha256(open(os.path.join(repo, "tests", "fonts", "Lato-Regular.ttf"), "rb").read()).hexdigest()


def at(x, y, **more):
    return {"x": x, "y": y, **more}


def solid(id, color, w, h, transform, **more):
    return {"id": id, "type": "solid", "color": color, "width": w, "height": h, "transform": transform, **more}


def keys(*pairs):
    out = []
    for p in pairs:
        key = {"t_ms": p[0], "v": p[1]}
        if len(p) > 2:
            key["ease"] = p[2]
        out.append(key)
    return {"keys": out}


def long_wav(rate, channels, seconds, freqs):
    """A long 16-bit WAV of one tone per channel, written quickly."""
    n = rate * seconds
    samples = array.array("h", bytes(2 * n * channels))
    for c, freq in enumerate(freqs):
        step = 2 * math.pi * freq / rate
        for k in range(n):
            samples[k * channels + c] = int(9000 * math.sin(step * k))
    if samples.itemsize != 2 or struct.pack("=h", 1) != struct.pack("<h", 1):
        raise SystemExit("needs a little-endian machine")
    data = samples.tobytes()
    return (b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVE"
            + b"fmt " + struct.pack("<IHHIIHH", 16, 1, channels, rate, rate * channels * 2, channels * 2, 16)
            + b"data" + struct.pack("<I", len(data)) + data)


def write_bench(folder):
    font = {"ref": {"family": "Lato", "style": "Regular", "sha256": lato}}
    benches = {
        "image-1024-text-blur": ({
            "output": {"kind": "image", "width": 1024, "height": 1024, "background": "#f4f0e8ff"},
            "assets": {"lato": font, "gradient": {"path": "assets/gradient.png"}},
            "layers": [
                {"id": "photo", "type": "image", "asset": "gradient", "width": 1024,
                 "transform": at(0, 256), "effects": [{"type": "blur", "sigma": 8}]},
                {"id": "headline", "type": "text", "text": "Unbaked benchmark: a headline that wraps onto two lines",
                 "font": "lato", "size_px": 72, "color": "#202020ff", "box_width": 900, "align": "center",
                 "transform": at(62, 40), "effects": [{"type": "shadow", "dx": 3, "dy": 3, "sigma": 2}]},
            ],
        }, {"gradient.png": ASSETS["gradient.png"]}),
        "image-1080-four-layers": ({
            "output": {"kind": "image", "width": 1920, "height": 1080},
            "assets": {"gradient": {"path": "assets/gradient.png"}, "badge": {"path": "assets/badge.png"}},
            "layers": [
                {"id": "background", "type": "image", "asset": "gradient", "width": 1920, "height": 1080},
                solid("tint", "#ff8040c0", 1200, 700, at(360, 190), blend="multiply"),
                {"id": "pair", "type": "group", "opacity": 0.6, "layers": [
                    solid("left", "#2040ffff", 500, 500, at(200, 300)),
                    solid("right", "#40ff80ff", 500, 500, at(450, 420), blend="screen")]},
                {"id": "badge", "type": "image", "asset": "badge", "width": 400,
                 "transform": at(1500, 540, anchor_x=0.5, anchor_y=0.5, rotation_deg=20)},
            ],
        }, {"gradient.png": ASSETS["gradient.png"], "badge.png": ASSETS["badge.png"]}),
        "audio-60s-three-clips": ({
            "output": {"kind": "audio", "duration_ms": 60000, "sample_rate": 48000, "channels": 2},
            "assets": {"voice": {"path": "assets/voice.wav"}, "music": {"path": "assets/music.wav"},
                       "effect": {"path": "assets/effect.wav"}},
            "audio": [
                {"id": "music", "asset": "music", "gain_db": keys((0, -6), (20000, -18, "ease-in-out"), (40000, -6))},
                {"id": "voice", "asset": "voice", "start_ms": 18000, "fade_in_ms": 500, "fade_out_ms": 500},
                {"id": "effect", "asset": "effect", "start_ms": 35000, "gain_db": -3},
            ],
        }, {"voice.wav": lambda: long_wav(44100, 1, 25, [220]),
            "music.wav": lambda: long_wav(48000, 2, 60, [330, 440]),
            "effect.wav": lambda: long_wav(22050, 1, 25, [880])}),
        "video-10s-1080p": ({
            "output": {"kind": "video", "width": 1920, "height": 1080, "fps": "30", "duration_ms": 10000},
            "assets": {"lato": font, "gradient": {"path": "assets/gradient.png"}, "badge": {"path": "assets/badge.png"}},
            "layers": [
                {"id": "background", "type": "image", "asset": "gradient", "width": 1920, "height": 1080},
                solid("mover", "#ffffffff", 200, 200, {"x": keys((0, 0, "ease-in-out"), (10000, 1720)), "y": 700,
                                                      "rotation_deg": keys((0, 0), (10000, 720))}),
                {"id": "badge", "type": "image", "asset": "badge", "width": 300, "start_ms": 2000, "end_ms": 8000,
                 "in": {"type": "fade", "duration_ms": 500}, "out": {"type": "zoom", "duration_ms": 500},
                 "transform": at(960, 400, anchor_x=0.5, anchor_y=0.5)},
                {"id": "title", "type": "text", "text": "Ten seconds at 1080p", "font": "lato", "size_px": 96,
                 "color": "#ffffffff", "transform": at(120, 80)},
            ],
        }, {"gradient.png": ASSETS["gradient.png"], "badge.png": ASSETS["badge.png"]}),
    }
    for name, (recipe, assets) in benches.items():
        target = os.path.join(folder, name)
        if os.path.exists(target):
            shutil.rmtree(target)
        os.makedirs(os.path.join(target, "assets"))
        for asset, make in assets.items():
            open(os.path.join(target, "assets", asset), "wb").write(make())
        with open(os.path.join(target, "recipe.json"), "w", newline="\n") as f:
            json.dump({"unbaked": 0, **recipe}, f, indent=2)
            f.write("\n")
        print("wrote", name)


if args.bench:
    write_bench(args.bench)
    raise SystemExit(0)

MODES = ["normal", "multiply", "screen", "overlay", "darken", "lighten",
         "color-dodge", "color-burn", "hard-light", "soft-light", "difference", "exclusion"]

CASES = {
    "image-blend-modes": ({
        "output": {"kind": "image", "width": 48, "height": 32, "background": "#202020ff"},
        "assets": {"gradient": {"path": "assets/gradient.png"}},
        "layers": [{"id": "base", "type": "image", "asset": "gradient"}]
        + [solid(f"mode-{m}", "#e0a040c8", 7, 7, at(0.5 + 8 * (i % 6), 4 + 14 * (i // 6)), blend=m)
           for i, m in enumerate(MODES)]
        + [solid("half", "#40c0ffff", 47, 3, at(0, 13.25), opacity=0.5)],
    }, ["gradient.png"]),
    "image-transforms": ({
        "output": {"kind": "image", "width": 64, "height": 48, "at_ms": 500},
        "assets": {"badge": {"path": "assets/badge.png"}, "gradient": {"path": "assets/gradient.png"}},
        "layers": [
            {"id": "big", "type": "image", "asset": "badge", "width": 24,
             "transform": at(16, 16, anchor_x=0.5, anchor_y=0.5, rotation_deg=30)},
            {"id": "small", "type": "image", "asset": "gradient", "width": 12, "transform": at(36, 4)},
            {"id": "squeezed", "type": "image", "asset": "badge", "width": 20, "height": 6,
             "transform": at(40, 30, rotation_deg=-15, scale_x=1.25, scale_y=0.75)},
            solid("moving", "#ff0000ff", 5.5, 5.5, {"x": keys((0, 0, "ease-in-out"), (1000, 60)), "y": 40,
                                                   "rotation_deg": keys((0, 0), (1000, 90))}),
            {"id": "corner", "type": "image", "asset": "badge",
             "transform": at(56, 8, anchor_x=1, anchor_y=0, rotation_deg=45)},
        ],
    }, ["badge.png", "gradient.png"]),
    "image-groups-masks": ({
        "output": {"kind": "image", "width": 48, "height": 48, "background": "#ffffffff", "at_ms": 300,
                   "duration_ms": 1000},
        "assets": {"badge": {"path": "assets/badge.png"}, "gradient": {"path": "assets/gradient.png"}},
        "layers": [
            {"id": "pair", "type": "group", "opacity": 0.5, "transform": at(4, 4, rotation_deg=10), "layers": [
                solid("red", "#ff0000ff", 16, 16, at(0, 0)),
                solid("blue", "#0000ffff", 16, 16, at(8, 8)),
            ]},
            {"id": "alpha-masked", "type": "image", "asset": "gradient", "width": 24, "transform": at(24, 0),
             "mask": {"mode": "alpha", "layers": [
                 {"id": "alpha-shape", "type": "image", "asset": "badge", "width": 24, "transform": at(24, 0)}]}},
            solid("luma-masked", "#008000ff", 24, 24, at(0, 24),
                  mask={"mode": "luminance", "invert": True, "layers": [
                      {"id": "luma-shape", "type": "image", "asset": "gradient", "width": 24,
                       "transform": at(0, 24)}]}),
            {"id": "timed", "type": "group", "start_ms": 100, "blend": "multiply",
             "effects": [{"type": "blur", "sigma": 1}], "layers": [
                 solid("late", "#ff8000ff", 20, 20, at(26, 26), start_ms=100, end_ms=500,
                       opacity=keys((0, 1), (300, 0.2)))]},
        ],
    }, ["badge.png", "gradient.png"]),
    "image-effects": ({
        "output": {"kind": "image", "width": 64, "height": 40, "background": "#406080ff"},
        "assets": {"badge": {"path": "assets/badge.png"}, "gradient": {"path": "assets/gradient.png"}},
        "layers": [
            {"id": "blurred", "type": "image", "asset": "badge", "width": 16, "transform": at(4, 4),
             "effects": [{"type": "blur", "sigma": 1.5}]},
            solid("shadowed", "#ffcc00ff", 14, 10, at(26, 6),
                  effects=[{"type": "shadow", "dx": 3, "dy": 2, "sigma": 1.2, "color": "#000000a0"}]),
            {"id": "adjusted", "type": "image", "asset": "gradient", "width": 20, "transform": at(42, 4),
             "effects": [{"type": "adjust", "brightness": 1.2, "contrast": 0.7, "saturation": 0.3}]},
            {"id": "chain", "type": "image", "asset": "badge", "width": 16, "transform": at(4, 22),
             "effects": [{"type": "adjust", "saturation": 0}, {"type": "blur", "sigma": 0.8},
                         {"type": "shadow", "dx": -2, "dy": 1, "color": "#ff00ff80"}]},
            {"id": "soft", "type": "group", "effects": [{"type": "blur", "sigma": 2}], "layers": [
                solid("left", "#ffffffff", 10, 10, at(30, 24)),
                solid("right", "#00ff80ff", 10, 10, at(36, 27), blend="difference")]},
        ],
    }, ["badge.png", "gradient.png"]),
    "image-text": ({
        "output": {"kind": "image", "width": 120, "height": 64, "background": "#f4f0e8ff"},
        "assets": {"lato": {"ref": {"family": "Lato", "style": "Regular", "sha256": lato}},
                   "gradient": {"path": "assets/gradient.png"}},
        "layers": [
            {"id": "title", "type": "text", "text": "Unbaked conformance", "font": "lato", "size_px": 14,
             "color": "#202020ff", "box_width": 80, "align": "center", "transform": at(4, 2)},
            {"id": "note", "type": "text", "text": "right\naligned", "font": "lato", "size_px": 9,
             "color": "#305090ff", "box_width": 30, "align": "right", "line_height": 1.4, "transform": at(88, 2)},
            {"id": "tilted", "type": "text", "text": "Tilt!", "font": "lato", "size_px": 18, "color": "#c03030ff",
             "transform": at(80, 46, anchor_x=0.5, anchor_y=0.5, rotation_deg=-15),
             "effects": [{"type": "shadow", "dx": 1, "dy": 1, "sigma": 0.5}]},
            {"id": "masked", "type": "image", "asset": "gradient", "width": 66, "transform": at(0, 32),
             "mask": {"mode": "alpha", "layers": [
                 {"id": "mask-word", "type": "text", "text": "MASK", "font": "lato", "size_px": 20,
                  "color": "#000000ff", "transform": at(4, 36)}]}},
        ],
    }, ["gradient.png"]),
    "image-webp": ({
        "output": {"kind": "image", "width": 40, "height": 24, "background": "#204060ff"},
        "assets": {"swatch": {"path": "assets/swatch.webp"}},
        "layers": [
            {"id": "native", "type": "image", "asset": "swatch", "transform": at(2, 2)},
            {"id": "scaled", "type": "image", "asset": "swatch", "width": 20, "transform": at(18, 2)},
        ],
    }, ["swatch.webp"]),
    "video-motion": ({
        "output": {"kind": "video", "width": 48, "height": 32, "fps": "10", "duration_ms": 1000},
        "assets": {"gradient": {"path": "assets/gradient.png"}},
        "layers": [
            {"id": "fader", "type": "image", "asset": "gradient", "width": 24, "start_ms": 150, "end_ms": 850,
             "in": {"type": "fade", "duration_ms": 250, "ease": "ease-out"},
             "out": {"type": "zoom", "duration_ms": 200},
             "transform": at(24, 20, anchor_x=0.5, anchor_y=0.5)},
            solid("bar", "#ffffffff", 8, 8, {
                "x": keys((0, 0, "ease-in-out"), (600, 40, "hold"), (800, 20)), "y": 8,
                "anchor_x": 0.5, "anchor_y": 0.5,
                "rotation_deg": keys((0, 0, [0.3, -0.6, 0.7, 1.6]), (1000, 90))}),
            solid("slider", "#00c0ffc0", 12, 6, at(30, 2), start_ms=100, end_ms=900,
                  **{"in": {"type": "slide-left", "duration_ms": 300},
                     "out": {"type": "slide-down", "duration_ms": 300, "ease": "ease-in"}}),
            {"id": "group", "type": "group", "start_ms": 300, "end_ms": 700,
             "in": {"type": "slide-up", "duration_ms": 200, "ease": "ease"},
             "out": {"type": "slide-right", "duration_ms": 150}, "layers": [
                 solid("dot", "#ff4040ff", 6, 6, at(2, 22), opacity=keys((0, 1), (300, 0.2))),
                 solid("late-dot", "#40ff40ff", 6, 6, at(10, 22), start_ms=200, end_ms=400)]},
        ],
    }, ["gradient.png"]),
    "video-clip": ({
        "output": {"kind": "video", "width": 48, "height": 32, "fps": "30000/1001", "duration_ms": 700},
        "assets": {"clip": {"path": "assets/clip.mp4"}},
        "layers": [
            {"id": "clip", "type": "video", "asset": "clip", "width": 48, "trim_start_ms": 200},
            {"id": "inset", "type": "video", "asset": "clip", "width": 16, "start_ms": 150, "end_ms": 650,
             "blend": "screen", "opacity": 0.8, "transform": at(30, 2)},
        ],
    }, ["clip.mp4"]),
    "video-with-sound": ({
        "output": {"kind": "video", "width": 32, "height": 18, "fps": "25", "duration_ms": 400,
                   "background": "#102040ff"},
        "assets": {"tone": {"path": "assets/tone-44k-mono.wav"}},
        "layers": [solid("mover", "#ffd000ff", 6, 6, {"x": keys((0, 0), (400, 26)), "y": 6})],
        "audio": [{"id": "tone", "asset": "tone", "gain_db": -6, "fade_out_ms": 100}],
    }, ["tone-44k-mono.wav"]),
    "audio-mix": ({
        "output": {"kind": "audio", "duration_ms": 300, "sample_rate": 48000, "channels": 2},
        "assets": {"tone": {"path": "assets/tone-44k-mono.wav"}, "chord": {"path": "assets/chord-48k-stereo.wav"},
                   "voice": {"path": "assets/voice.m4a"}},
        "audio": [
            {"id": "tone", "asset": "tone", "start_ms": 20, "fade_in_ms": 30, "fade_out_ms": 50,
             "gain_db": keys((0, -6, "ease-in"), (150, 0))},
            {"id": "chord", "asset": "chord", "start_ms": 100, "trim_start_ms": 50, "duration_ms": 150,
             "gain_db": -3},
            {"id": "voice", "asset": "voice", "gain_db": -9},
            {"id": "loud", "asset": "chord", "start_ms": 250, "gain_db": 12},
            {"id": "muted", "asset": "tone", "muted": True},
        ],
    }, ["tone-44k-mono.wav", "chord-48k-stereo.wav", "voice.m4a"]),
    "audio-mono": ({
        "output": {"kind": "audio", "duration_ms": 200, "sample_rate": 44100, "channels": 1},
        "assets": {"chord": {"path": "assets/chord-48k-stereo.wav"}, "voice": {"path": "assets/voice.m4a"}},
        "audio": [
            {"id": "chord", "asset": "chord", "gain_db": -3},
            {"id": "voice", "asset": "voice", "start_ms": 40, "trim_start_ms": 30, "fade_in_ms": 20},
        ],
    }, ["chord-48k-stereo.wav", "voice.m4a"]),
}

for name, (recipe, assets) in CASES.items():
    package = os.path.join(root, name, "package")
    if os.path.exists(package):
        shutil.rmtree(package)
    os.makedirs(os.path.join(package, "assets"))
    for asset in assets:
        target = os.path.join(package, "assets", asset)
        if asset == "clip.mp4":
            shutil.copyfile(os.path.join(repo, "tests", "video", "frames-high.mp4"), target)
        elif asset == "voice.m4a":
            open(target, "wb").write(voice_m4a)
        else:
            open(target, "wb").write(ASSETS[asset]())
    with open(os.path.join(package, "recipe.json"), "w", newline="\n") as f:
        json.dump({"unbaked": 0, **recipe}, f, indent=2)
        f.write("\n")
    print("wrote", name)

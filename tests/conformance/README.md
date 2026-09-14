# Conformance cases

Each folder is one case for SPEC.md section 8. A renderer conforms when its
output matches every case within that section's tolerances.

```
<case>/
  package/            the package in directory form (section 3.3)
  expected/
    frame-NNNN.png    frame N before encoding, 8-bit RGBA
    cover-NNNN.png    text cases: opaque where frame N's pixels are covered by text
    glyphs.json       text cases: glyph origins per text layer
    audio.wav         cases with sound: the mix before encoding, 32-bit float
```

- **Frames.** `image` cases have one frame, `frame-0000.png`, at `output.at_ms`, in straight colour. `video` cases have every frame, composited over opaque black, with alpha 255. Pixels that are opaque in the matching `cover` file are not compared.
- **Glyphs.** `{"layers": {"<layer id>": [{"glyph": id, "x": px, "y": px}, …]}}`, glyphs in visual order line by line, positions in the layer box as section 8 describes.
- **Sound.** Samples are interleaved; the file has a `fact` chunk and an 18-byte `fmt ` chunk (format 3).

Referenced fonts are in `tests/fonts/`, matched by SHA-256.

| Case | What it covers |
|---|---|
| `image-blend-modes` | The 12 blend modes, opacity, half-pixel edges |
| `image-transforms` | Bilinear upscaling, mip chain downscaling, rotation, anchors, aspect fit, keyframes at `at_ms` |
| `image-groups-masks` | Isolated group opacity, alpha and inverted luminance masks, nested timing, group effects |
| `image-effects` | Blur, shadow, adjust, effect chains, group blur with `difference` |
| `image-jpeg-exif` | A baseline JPEG with EXIF orientation 6, at its own size and stretched |
| `image-text-rtl` | Right-to-left Arabic with joined letters, a number inside it, wrapping and right alignment |
| `image-webp` | A lossless WebP with alpha, at its own size and scaled |
| `image-text` | Wrapping, centre and right alignment, line height, rotation, shadow, text as a mask |
| `video-motion` | Easing curves including an overshooting Bézier, `hold`, every transition type, nested start and end |
| `video-clip` | Video layers with trim and start at 30000/1001 fps, one clip shown at two times |
| `video-with-sound` | Frames and sound together |
| `audio-mix` | Resampling 44.1 kHz to 48 kHz, mono to stereo, gain keyframes, fades, trim, duration, mute, hard clipping, an M4A source with an edit list |
| `audio-mono` | Stereo to mono, 48 kHz to 44.1 kHz |

The PNG, JPEG, WebP and WAV assets are small generated patterns; the generator
writes the JPEG and WebP encoders out by hand. `clip.mp4` is
`tests/video/frames-high.mp4`, and `voice.m4a` was rendered by this renderer
from a 660 Hz tone.

## Making the cases

`tools/make_cases.py` writes every `package` folder, recipes and assets, and
a run reproduces the committed files byte for byte. Edit a case there, not by
hand. It needs only Python 3.

```sh
python tests/conformance/tools/make_cases.py
```

`voice.m4a` is kept, not regenerated. `--voice-recipe DIR` writes the recipe it
was rendered from; render that and pass the result with `--voice`.

`--bench DIR` writes four full-size jobs instead (a 1024×1024 image with text
and a blur, a 1920×1080 four-layer image, a 60 s stereo mix of three clips and
a 10 s 1080p video); `.github/workflows/bench.yml` times them in a release
build.

`tools/sheet.py OUT SCALE PNG...` stacks PNGs, enlarged, into one image over a
checkerboard, for looking at expected frames.

## Updating the references

The test in `crates/unbaked-render/tests/conformance.rs` renders every case
and compares. After a deliberate change to rendering, rewrite the references
and review the difference before committing:

```sh
UNBAKED_BLESS=1 cargo test -p unbaked-render --test conformance
```

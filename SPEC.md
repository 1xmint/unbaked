# Unbaked format specification

**Version 0 (draft).** Nothing in version 0 is stable. Version 1 will be the
first version with compatibility promises.

## 0. Conventions

**MUST**, **MUST NOT**, **SHOULD** and **MAY** mean what RFC 2119 says: MUST is
a hard rule, SHOULD is a strong default you need a reason to break, MAY is
optional.

- A **writer** creates or updates Unbaked files.
- A **reader** opens them.
- A **renderer** turns a recipe into finished media.
- All JSON is UTF-8, RFC 8259.
- All hashes are SHA-256, written as 64 lowercase hex characters.

---

## 1. Overview

An Unbaked file is an ordinary media file (the **carrier**) whose normal content
is the finished render. Inside it, in a place players are required to skip, is a
ZIP archive (the **package**) holding the recipe and source assets that produce
that render.

```
carrier (.png / .m4a / .mp4)
├── normal media content    ← what every player shows: the last render
└── hidden slot
    └── package (ZIP)
        ├── recipe.json     ← how to build the render
        ├── bake.json       ← fingerprints from the last render
        └── assets/…        ← source images, video, audio, fonts
```

---

## 2. Carriers

The recipe's `output.kind` decides the carrier.

| `output.kind` | Carrier | File name ending | Media content |
|---|---|---|---|
| `image` | PNG | `.unbaked.png` | 8-bit RGBA, sRGB |
| `audio` | MP4 audio | `.unbaked.m4a` | AAC-LC |
| `video` | MP4 | `.unbaked.mp4` | H.264 video, plus AAC-LC audio if the recipe has audio |

A picture with sound is `video`.

Writers SHOULD use the file name endings above. Readers MUST NOT rely on the
file name; they detect the carrier from its first bytes.

### 2.1 PNG slot

The package is stored in one chunk of type **`unBK`**.

The chunk-type letters encode, per the PNG spec: ancillary (`u`, lowercase),
private (`n`, lowercase), reserved bit clear (`B`, uppercase), and **not safe to
copy** (`K`, uppercase). "Not safe to copy" tells image editors to drop the
chunk if they change the image. A re-edited image then no longer claims layers
that don't match it.

- Chunk data is the complete ZIP archive, byte for byte.
- A file MUST contain at most one `unBK` chunk. Readers MUST reject files with more than one.
- Writers MUST place the chunk after the last `IDAT` chunk and immediately before `IEND`.
- Readers MUST accept the chunk anywhere between `IHDR` and `IEND`.
- PNG caps chunk data at 2³¹−1 bytes. A package larger than that cannot use a PNG carrier.

### 2.2 MP4 / M4A slot

The package is stored in one top-level ISO BMFF `uuid` box whose 16-byte
extended type is:

```
a7094a3c-3a0b-494c-ba92-0dd1b181e4e0
bytes: a7 09 4a 3c 3a 0b 49 4c ba 92 0d d1 b1 81 e4 e0
```

- Box payload (after size, `uuid` and the extended type) is the complete ZIP archive.
- Writers MUST use the 64-bit `largesize` form when the box exceeds 2³²−1 bytes.
- A file MUST contain at most one such box. Readers MUST reject files with more than one.
- Writers MUST place the box as the last top-level box. Appending a box at the end does not move any media data, so no sample offsets change.
- Readers MUST accept the box at any top-level position.

---

## 3. Package

The package is a ZIP archive (APPNOTE 6.3.x).

### 3.1 Contents

| Path | Required | Purpose |
|---|---|---|
| `recipe.json` | yes | Section 4 |
| `bake.json` | yes | Section 7 |
| `assets/` | if the recipe references assets | Source files |

Anything else at the root is reserved. Readers MUST ignore unknown root entries
whose names start with `x-`, and MUST reject any other unknown root entry.

### 3.2 ZIP rules

Writers MUST:
- use compression method 0 (stored) for media and font assets, and 0 or 8 (deflate) for everything else;
- encode names as UTF-8, set the UTF-8 flag (bit 11), and use `/` as the separator;
- use ZIP64 when any size or offset needs it.

Readers MUST reject a package that contains:
- an entry name that is empty, starts with `/`, contains `\`, contains a `..` or `.` path segment, contains a drive letter (`C:`), or contains a NUL;
- two entries whose names are equal after Unicode NFC normalisation and case folding (Windows and macOS treat those as the same file);
- encryption, compression methods other than 0 or 8, symbolic links, or multi-disk archives;
- an entry whose decompressed size does not match its declared size.

Readers SHOULD apply size limits and report which limit was hit rather than
running out of memory. Suggested defaults: 4 GiB total decompressed, 100:1
compression ratio per deflated entry.

### 3.3 Directory form

A package MAY also exist as a plain folder with the same layout, for editing and
for tools. The directory form is not a file format and has no carrier or render.
`bake.json` is optional in the directory form, because nothing has been rendered
yet. Rendering it produces a carrier with a fresh `bake.json`.

---

## 4. recipe.json

### 4.1 Top level

```json
{
  "unbaked": 0,
  "output": { "kind": "video", "width": 1920, "height": 1080, "fps": "30", "duration_ms": 8000 },
  "assets": { "bg": { "path": "assets/beach.mp4" }, "title_font": { "path": "assets/Inter.ttf" } },
  "layers": [],
  "audio": []
}
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `unbaked` | integer | yes | Spec version. Readers MUST reject versions they do not implement. |
| `output` | object | yes | Section 4.2 |
| `assets` | object | yes, may be empty | Asset id → asset, section 4.3 |
| `layers` | array | yes for `image` and `video`; MUST be absent for `audio` | Visual layers, bottom first, section 4.5 |
| `audio` | array | MUST be absent for `image` | Audio clips, section 4.7 |

**Unknown fields.** A renderer that ignores a field it doesn't understand draws
the wrong picture without any warning. So readers MUST reject any object field
this spec does not define, at any depth, unless its name starts with `x-`.
Readers MUST ignore `x-` fields. Renderers MUST NOT let `x-` fields change output.

**Ids.** Asset ids, layer ids and clip ids MUST match `^[A-Za-z0-9_-]{1,64}$`.
Layer and clip ids MUST be unique across the whole recipe, including layers
inside groups and masks.

**Written for AI editing.** Every field has one type and, where it has a unit,
the unit is in its name (`_ms`, `_px`, `_deg`, `_db`). Tools that accept
friendlier input such as `"1m30s"` convert it before writing the recipe.

**Schema.** [`schema/recipe.schema.json`](schema/recipe.schema.json) (JSON Schema
2020-12) describes this section's structure. A recipe that fails the schema is
invalid. Passing it is not enough: readers MUST also check the rules the schema
lists in its `$comment`. Where the schema and this text disagree, this text wins.

### 4.2 output

| Field | `image` | `audio` | `video` | Meaning |
|---|---|---|---|---|
| `kind` | required | required | required | `"image"`, `"audio"` or `"video"` |
| `width`, `height` | required | absent | required | Canvas size in pixels, 1–16384. For `video`, both MUST be even. |
| `background` | optional | absent | optional | Color, section 4.4. Default `"#00000000"` for `image`, `"#000000ff"` for `video`. |
| `fps` | absent | absent | required | Frame rate as a string: `"30"` or `"30000/1001"`. Both numbers are positive integers. |
| `duration_ms` | absent | required | required | Length in milliseconds, positive integer |
| `sample_rate` | absent | optional | optional | `44100` or `48000`. Default `48000`. |
| `channels` | absent | optional | optional | `1` or `2`. Default `2`. |

For `video` with an opaque background, the renderer SHOULD treat the final frame
as opaque. H.264 output has no alpha.

### 4.3 assets

```json
"assets": {
  "logo": { "path": "assets/logo.png" }
}
```

| Field | Type | Required | Meaning |
|---|---|---|---|
| `path` | string | yes, unless `ref` | Package path (section 3.2 rules) under `assets/`. The file MUST exist. |
| `ref` | object | fonts only, instead of `path` | A font that is not packed, section 4.12 |
| `license` | object | yes for packed fonts, otherwise optional | Section 4.12 |

The asset's kind is detected from the file's bytes, not its name. An asset MUST
have exactly one of `path` and `ref`.

Every renderer MUST support at least:

| Kind | Formats |
|---|---|
| Image | PNG, JPEG |
| Video | MP4 containing H.264 |
| Audio | M4A/MP4 containing AAC-LC, MP3, WAV (PCM 16/24-bit, float 32-bit), FLAC |
| Font | TrueType (`.ttf`), OpenType (`.otf`) |

A package MAY contain files that no asset references. Readers MUST NOT fail
because of them. Writers SHOULD remove them.

### 4.4 Shared value types

**Time.** Integer milliseconds, named `*_ms`, ≥ 0.

**Color.** A string `"#RRGGBB"` or `"#RRGGBBAA"`: sRGB, not premultiplied, hex
digits in either case. `AA` defaults to `ff`.

**Local time.** Every time inside a layer or clip is measured from that layer's
or clip's own `start_ms`, and a nested layer's `start_ms` is measured from its
parent's `start_ms`. Moving a layer or group in time moves everything inside it.

**Animatable number.** Fields marked *animatable* take either a plain number or
a keyframe object (section 4.8):

```json
"opacity": 1
"opacity": { "keys": [ { "t_ms": 0, "v": 0, "ease": "ease-out" }, { "t_ms": 500, "v": 1 } ] }
```

**Transform.** An optional object on every visual layer. All fields are animatable.

| Field | Type | Default | Meaning |
|---|---|---|---|
| `x`, `y` | number | `0` | Canvas position of the anchor, in pixels. Origin top-left, y points down. |
| `anchor_x`, `anchor_y` | number | `0` | Anchor point as a fraction of the layer's own box. `0,0` top-left, `0.5,0.5` centre. |
| `scale_x`, `scale_y` | number | `1` | Scale about the anchor. MUST NOT be negative at any time. At `0` the layer draws nothing. |
| `rotation_deg` | number | `0` | Clockwise rotation about the anchor |

The layer box is scaled, then rotated, then its anchor is placed at `x,y`. Inside
a group, the result is then transformed by the group's transform, and so on
outward.

### 4.5 Visual layers

`layers` lists layers from bottom to top. Every layer has:

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `id` | string | yes | | Unique id |
| `type` | string | yes | | `image`, `video`, `text`, `solid` or `group` |
| `start_ms` | integer | no | `0` | First visible time, from the parent's start (`video` only) |
| `end_ms` | integer | no | parent's end | Visible until, exclusive, from the parent's start (`video` only) |
| `transform` | object | no | identity | Section 4.4 |
| `opacity` | animatable | no | `1` | 0–1 |
| `blend` | string | no | `"normal"` | Section 5.4 |
| `effects` | array | no | `[]` | Applied in order, section 5.5 |
| `mask` | object | no | | Section 4.11 |
| `in`, `out` | object | no | | Transitions, section 4.9 (`video` only) |
| `hidden` | boolean | no | `false` | Skipped entirely when `true` |

For `image` output, `start_ms`, `end_ms`, `in`, `out` and keyframe objects MUST
be absent. For `video` output, `start_ms` MUST be less than `end_ms`. A child is
never visible outside its parent's visible time.

Type-specific fields:

**`image`**

| Field | Required | Meaning |
|---|---|---|
| `asset` | yes | Id of an image asset |
| `width`, `height` | no | Box size in pixels. If one is given, the other follows the image's aspect ratio. If neither is given, the image's own size. |

**`video`** (only in `video` output)

| Field | Required | Default | Meaning |
|---|---|---|---|
| `asset` | yes | | Id of a video asset |
| `width`, `height` | no | | Same rule as `image` |
| `trim_start_ms` | no | `0` | Point in the source shown at the layer's `start_ms` |

A video layer never contributes sound. To use a video's sound, add an audio clip
that references the same asset. Past the end of the source, the last frame is held.

**`text`**

| Field | Required | Default | Meaning |
|---|---|---|---|
| `text` | yes | | The string. `\n` forces a line break. |
| `font` | yes | | Id of a font asset |
| `size_px` | yes | | Font size in pixels (em size), > 0 |
| `color` | no | `"#000000ff"` | Fill color |
| `line_height` | no | `1.2` | Multiple of `size_px` between baselines |
| `align` | no | `"left"` | `left`, `center` or `right`, within the box |
| `box_width` | no | | Wrap width in pixels. Without it, lines only break at `\n`. |
| `font_index` | no | `0` | Face index for font collections |

System fonts are never used by name. A text layer draws only with a packed font
or a referenced font whose fingerprint matches (section 4.12).

**`solid`**

| Field | Required | Meaning |
|---|---|---|
| `color` | yes | Fill color |
| `width`, `height` | yes | Box size in pixels |

**`group`**: see section 4.10.

| Field | Required | Meaning |
|---|---|---|
| `layers` | yes | Child layers, bottom first. Same rules as top-level `layers`. |

### 4.6 Layer box

Before any transform, every layer occupies a box with its top-left at `0,0`:

- `image` / `video`: the size from `width`/`height` rules above.
- `solid`: `width` × `height`.
- `group`: the canvas, `output.width` × `output.height`.
- `text`:
  - width is `box_width` if given, otherwise the widest line's advance width;
  - height is `line_height × size_px × line count`;
  - the first baseline sits at `(line_height × size_px − (ascender + descender)) / 2 + ascender` from the top, using the font's `hhea` ascender and descender magnitudes.

### 4.7 Audio clips

`audio` lists clips. Order does not matter; clips are mixed.

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `id` | string | yes | | Unique id |
| `asset` | string | yes | | Id of an audio or video asset. For a video asset, its first audio track. |
| `start_ms` | integer | no | `0` | Output time where the clip begins |
| `trim_start_ms` | integer | no | `0` | Source time played at `start_ms` |
| `duration_ms` | integer | no | rest of source | How long the clip plays |
| `gain_db` | animatable | no | `0` | Volume change in decibels. Keyframe times are from the clip's `start_ms`. |
| `fade_in_ms` | integer | no | `0` | Fade from silence at the start |
| `fade_out_ms` | integer | no | `0` | Fade to silence at the end |
| `muted` | boolean | no | `false` | Skipped entirely when `true` |

Parts of a clip past `output.duration_ms` are cut off. Clips produce silence
past the end of their source.

### 4.8 Keyframes

A keyframe object makes a number change over time.

```json
"x": { "keys": [
  { "t_ms": 0,    "v": -400, "ease": "ease-out" },
  { "t_ms": 600,  "v": 100 },
  { "t_ms": 3000, "v": 100,  "ease": "hold" },
  { "t_ms": 3001, "v": 900 }
] }
```

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `keys` | array | yes | | At least one key, in strictly increasing `t_ms` |
| `keys[].t_ms` | integer | yes | | Local time of the key (section 4.4) |
| `keys[].v` | number | yes | | Value at that time |
| `keys[].ease` | string or array | no | `"linear"` | How the value moves from this key to the next |

**Easing.** One of:
- `"linear"`
- `"hold"`: stays at `v` until the next key
- `"ease"`, `"ease-in"`, `"ease-out"`, `"ease-in-out"`: the cubic Bézier curves that **W3C CSS Easing Functions Level 1** gives these keywords
- `[x1, y1, x2, y2]`: a cubic Bézier with `0 ≤ x1, x2 ≤ 1`, as CSS `cubic-bezier()` defines it

**Value at local time `t`** (may be fractional, section 5.2):
- before the first key: the first key's `v`;
- after the last key: the last key's `v`;
- between key `a` and the next key `b`: `p = (t − a.t_ms) / (b.t_ms − a.t_ms)`, then `v = a.v + (b.v − a.v) × E(p)`, where `E` is `a.ease`. For Bézier curves, the renderer solves for the curve parameter to within 10⁻⁶.

Animatable fields are: every `transform` field, `opacity`, the number parameters
of effects (section 5.5), and `gain_db`. No other field accepts a keyframe object.

### 4.9 Transitions

`in` and `out` are shortcuts for common keyframe patterns at the start and end of
a layer. A renderer computes them as multipliers and offsets on top of the
layer's own values, so transitions work together with keyframes.

```json
"in":  { "type": "slide-left", "duration_ms": 400, "ease": "ease-out" },
"out": { "type": "fade", "duration_ms": 300 }
```

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `type` | string | yes | | Preset name, below |
| `duration_ms` | integer | yes | | Length, > 0, and no longer than the layer |
| `ease` | string or array | no | `"linear"` | Easing as in section 4.8. `"hold"` is not allowed. |

**Progress.** For `in`, `p = E(clamp(t / duration_ms, 0, 1))` with local time
`t`. For `out`, `p = E(clamp((end − t) / duration_ms, 0, 1))`, where `end` is the
layer's visible length. `p = 1` means fully shown.

The name says which way the layer moves, for both `in` and `out`. With
`W = output.width`, `H = output.height` and `q = 1 − p`:

| `type` | As `in` | As `out` |
|---|---|---|
| `fade` | opacity × `p` | opacity × `p` |
| `slide-left` | x + `q·W` (enters from the right) | x − `q·W` (leaves to the left) |
| `slide-right` | x − `q·W` (enters from the left) | x + `q·W` (leaves to the right) |
| `slide-up` | y + `q·H` (enters from below) | y − `q·H` (leaves upward) |
| `slide-down` | y − `q·H` (enters from above) | y + `q·H` (leaves downward) |
| `zoom` | scale_x, scale_y and opacity × `p` | scale_x, scale_y and opacity × `p` |

Where `in` and `out` overlap in time, both apply.

A crossfade is two overlapping layers: `out: fade` on the first and `in: fade` on
the second.

### 4.10 Groups

A `group` layer holds child layers and treats them as one layer.

- Children render bottom to top into a transparent, canvas-sized buffer (the group is **isolated**: children blend with each other, not with what is below the group).
- Each child's transform is combined with the group's transform (section 4.4).
- The group's `effects` then apply to that buffer, in canvas coordinates. Pixels pushed outside the canvas by a group effect are lost.
- The group's `mask`, `opacity` and `blend` then apply as for any layer.
- Groups MAY contain groups. Readers MUST reject nesting deeper than 32 levels.

### 4.11 Masks

A mask controls where a layer shows. Its contents are their own small list of
layers that are never drawn to the canvas themselves.

```json
"mask": {
  "mode": "alpha",
  "invert": false,
  "layers": [
    { "id": "spot", "type": "solid", "color": "#ffffffff", "width": 600, "height": 600,
      "transform": { "x": 960, "y": 540, "anchor_x": 0.5, "anchor_y": 0.5 } }
  ]
}
```

| Field | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `layers` | array | yes | | Mask layers, bottom first. Same rules as `layers`. |
| `mode` | string | no | `"alpha"` | `alpha` or `luminance` |
| `invert` | boolean | no | `false` | Use `1 − m` instead of `m` |

- Mask layers are rendered like a group (section 4.10) into their own canvas-sized buffer. They are placed by their own transforms and by the transforms of the groups enclosing the masked layer, but not by the masked layer's own transform. Their times are from the masked layer's absolute start.
- Per pixel, the mask value `m` is:
  - `alpha`: the buffer's alpha;
  - `luminance`: `(0.2126·R + 0.7152·G + 0.0722·B) × A`, on un-premultiplied gamma-encoded values.
- The masked layer's transformed, effected pixels are multiplied by `m` (color and alpha, premultiplied) before opacity and blending.
- A mask layer MAY itself have a mask.

### 4.12 Fonts and licences

**Packed fonts** have a `path` and MUST have a `license`:

```json
"inter": {
  "path": "assets/fonts/Inter-Regular.ttf",
  "license": { "spdx": "OFL-1.1", "file": "assets/fonts/OFL.txt" }
}
```

| `license` field | Type | Meaning |
|---|---|---|
| `spdx` | string | An SPDX licence id (`OFL-1.1`, `Apache-2.0`), or `LicenseRef-` followed by a name for anything else |
| `file` | string | Package path of the licence text |

- At least one of `spdx` and `file` MUST be present.
- When `spdx` is `OFL-1.1` or `Apache-2.0`, `file` MUST be present too, because both licences require their text to travel with the font.
- Writers MUST pack the complete font file, not a subset. The OFL counts subsetting as modification, and modified fonts may lose the right to their name.
- Any asset MAY carry a `license` in the same shape.
- Readers do not judge whether a licence allows packing. That is the author's responsibility.

**Referenced fonts** have a `ref` instead of a `path`. The font file is not in the
package: its licence may not allow handing it out.

```json
"brand": {
  "ref": { "family": "Brand Sans", "style": "Bold", "sha256": "…" }
}
```

| `ref` field | Required | Meaning |
|---|---|---|
| `family` | yes | Family name, for people |
| `style` | no | Style name, for people |
| `sha256` | yes | SHA-256 of the exact font file |

- The last render still shows everywhere, because the text is already in the pixels.
- To render again, a renderer MUST find a font file with the same SHA-256 from a source its user controls, such as a local fonts folder. Matching by family name alone is not allowed.
- If no match is found, rendering MUST fail with an error naming `family` and `style`. Reading, unpacking and checking still work.

---

## 5. Rendering visuals

The goal is that two correct renderers produce the same pixels. Section 8 says
how close "the same" has to be.

### 5.1 Pixel values

- All image data is treated as sRGB. Embedded ICC profiles, `gAMA`, `cHRM` and `iCCP` are ignored. Authors who need color accuracy supply sRGB assets.
- Samples are converted to floating point 0–1 by dividing by the format's maximum value (255 for 8-bit, 65535 for 16-bit).
- Compositing happens on these gamma-encoded values, not on linear light. This matches what CSS and most image editors do by default.
- During rendering, colors are held **premultiplied** (color multiplied by alpha).
- JPEG images with an EXIF orientation are rotated to their display orientation first.

### 5.2 Video frames

Output frame `n` (starting at 0) has time `tₙ = n × den / num` seconds for
`fps = "num/den"`. There are `ceil(duration_ms × num / (1000 × den))` frames.

**Absolute times.** A layer's absolute start `S` is its `start_ms` plus the
absolute start of its parent (a group, or the layer a mask belongs to), and `0`
at the top level. Its absolute end `E` is `S − start_ms + end_ms`, capped at the
parent's absolute end, with `output.duration_ms` at the top level.

A layer is visible on frame `n` when `S ≤ 1000·tₙ < E`. Compare with integer
arithmetic: `S × num ≤ 1000 × n × den < E × num`.

**Local time** on frame `n` is `t = 1000·tₙ − S` ms. It may be fractional.
Keyframes and transitions are evaluated at `t`. The visible length used by `out`
is `E − S`.

For a visible video layer, the source time is `s = t + trim_start_ms` ms. The
renderer shows the source frame with the greatest presentation time ≤ `s`.

For `image` output, there is a single frame and every layer is visible.

Source frames are converted from YUV to RGB using the matrix and range the
stream signals. If none is signalled: BT.709 limited range when the height is
≥ 720, BT.601 limited range otherwise. The stream's display-rotation metadata is
applied.

### 5.3 Transform and sampling

Each layer's full transform `M` is an affine matrix mapping source-image pixels
to canvas pixels: source image to layer box, then the layer's transform, then
each enclosing group's transform, innermost first. Transform values (after
keyframes and transitions) are those at the frame's local time of each layer.

Each output pixel centre `(px + 0.5, py + 0.5)` is mapped back through `M⁻¹`.
The layer's source image (the decoded image, video frame, rendered text or solid
fill, after effects) is sampled there.

**Sampling.** Let `s` be the smaller of the lengths of `M`'s two column vectors:
how many canvas pixels one source pixel covers along each source axis.

- If `s ≥ 1`: bilinear interpolation. Source pixel centres are at `+0.5`. Outside the source image, pixels are transparent.
- If `s < 1`: build a mip chain first. Level 0 is the source. Each next level halves width and height (rounding up), and every pixel is the average of the up-to-4 premultiplied pixels it covers, ignoring pixels outside the image. Sample level `L = min(floor(log2(1/s)), last level)` bilinearly.

### 5.4 Compositing

Canvas starts as `output.background`. Each non-hidden, visible layer, bottom
first, goes through these steps:

1. **Source image.** Decode the image or video frame, draw the text, or fill the solid. A group renders its children into a transparent canvas-sized buffer by these same steps (section 4.10).
2. **Effects**, in order (section 5.5). For a group, on its buffer.
3. **Place.** Sample into a transparent canvas-sized buffer through `M` (section 5.3). A group's buffer is already in canvas space and is used as is.
4. **Mask.** Multiply by the mask value `m` per pixel (section 4.11).
5. **Opacity.** Multiply by `opacity`, including transition factors.
6. **Blend** the result onto what is below, source-over, using `blend`.

Blend modes and their formulas are exactly the separable blend modes of **W3C
Compositing and Blending Level 1**, combined with the source-over operator as
that spec defines the general blending-and-compositing formula:

`normal`, `multiply`, `screen`, `overlay`, `darken`, `lighten`, `color-dodge`,
`color-burn`, `hard-light`, `soft-light`, `difference`, `exclusion`

### 5.5 Effects

Effects change a layer's own pixels before transform and compositing, in array
order. Each is an object with a `type`. Every number parameter below is
animatable; `color` is not.

**`blur`**: `sigma` (pixels, > 0).
- Gaussian blur on premultiplied colour, kernel radius `ceil(3 × sigma)`, weights normalised to sum to 1, applied horizontally then vertically.
- The layer image first grows by the kernel radius on every side, filled transparent, so the blur can spread outward. Content stays where it was: `anchor_x`, `anchor_y` and the box size in section 4.6 still refer to the original box, and the extra margin simply extends past it.

**`shadow`**: `dx`, `dy` (pixels, default `0`), `sigma` (≥ 0, default `0`), `color` (default `"#00000080"`).
- Takes the layer's alpha, fills it with `color` (alpha multiplied), blurs it by `sigma` (as `blur`), offsets it by `dx, dy`, and draws the layer over it source-over. The image grows to fit, as with `blur`.

**`adjust`**: `brightness`, `contrast`, `saturation` (all numbers, default `1`).
- Applied to un-premultiplied colour, in that order.
- Formulas are the SVG equivalents that **W3C Filter Effects Module Level 1** gives for the `brightness()`, `contrast()` and `saturate()` filter functions.
- Results are clamped to 0–1 after each step.

### 5.6 Text

- Text is shaped with OpenType shaping (HarfBuzz behaviour is the reference): default features, direction and script detected per run.
- With `box_width`, lines wrap at Unicode UAX #14 break opportunities; a word wider than the box is not split.
- Each line is positioned within the box by `align`; lines step down by `line_height × size_px`.
- Glyph outlines are filled with the non-zero rule and anti-aliased by exact area coverage.

### 5.7 Final output

- **`image`**: premultiplied values are un-premultiplied (a pixel with alpha 0 becomes all zeros), then quantised with `round(v × 255)`, rounding halves up, and clamped to 0–255. The PNG is 8-bit RGBA with an `sRGB` chunk.
- **`video`**: each frame is composited over opaque black, quantised the same way, and is converted to YUV 4:2:0, BT.709 limited range, H.264. Encoder settings are not specified. The encoder is lossy, so section 8 compares frames before encoding.

---

## 6. Rendering audio

1. Each non-muted clip is decoded to floating point −1–1.
2. It is resampled to `output.sample_rate`.
3. Channels are mapped: mono to stereo copies the channel; stereo to mono averages the two. Sources with more than 2 channels are rejected in version 0.
4. Gain `10^(gain_db / 20)` is applied per sample, with `gain_db` evaluated at the sample's local time `1000·k / sample_rate − start_ms`.
5. Fades are linear in amplitude. Fade-in scales by `t / fade_in_ms` over the first `fade_in_ms`. Fade-out mirrors it at the end.
6. Output sample `k` has time `k / sample_rate` s. A clip contributes to sample `k` when `start_ms ≤ 1000·k / sample_rate < start_ms + duration_ms`.
7. All clips are summed, then hard-clipped to −1–1.
8. The result is encoded AAC-LC. Encoder settings are not specified.

The resampling filter is not specified in version 0. Section 8 allows for the
difference.

---

## 7. bake.json

bake.json records what the render was made from, so any tool can tell whether
the render is out of date without rendering.

```json
{
  "unbaked": 0,
  "renderer": "unbaked-render 0.1.0",
  "recipe_sha256": "…",
  "assets_sha256": { "assets/logo.png": "…" },
  "render_sha256": "…"
}
```

| Field | Meaning |
|---|---|
| `unbaked` | Spec version used |
| `renderer` | Free text naming the renderer and its version |
| `recipe_sha256` | SHA-256 of `recipe.json` exactly as stored (uncompressed bytes) |
| `assets_sha256` | Package path → SHA-256, for every file an asset's `path` or `license.file` references. Referenced fonts are not listed: their fingerprint is already in the recipe. |
| `render_sha256` | SHA-256 of the carrier with the slot removed (below) |

**Carrier with the slot removed:**
- PNG: the file bytes with the whole `unBK` chunk (length, type, data, CRC) cut out.
- MP4: the file bytes with the whole `uuid` box cut out.

**Checking a file.** A reader compares hashes and reports one of:

| Result | When |
|---|---|
| `fresh` | Every hash matches |
| `stale` | `recipe_sha256` or any asset hash differs, or an asset was added or removed. The layers changed but the render is old. |
| `render-modified` | Recipe and assets match but `render_sha256` differs. Something edited the visible media outside Unbaked. |

`stale` wins if both apply. Writers MUST write a new bake.json every time they render.

---

## 8. Conformance

Bit-exact output across independent renderers is not realistic for resampling,
text anti-aliasing and lossy encoders. Version 0 defines "matching" as:

- **Image and video frames**, compared before encoding: every channel within ±2 of the reference for at least 99.9% of pixels, excluding pixels covered by text layers.
- **Text**: glyph positions within 0.5 px of the reference.
- **Audio**, compared before encoding: difference signal at least 60 dB below the reference signal.

The reference renderer is the one in this repository. Conformance test cases
will live in `tests/conformance/` (not written yet).

---

## 9. Security

Unbaked files come from strangers. Readers and renderers MUST treat every byte
as hostile:

- Enforce the ZIP rules in section 3.2 before extracting anything.
- Never write package paths to disk without the section 3.2 checks.
- Bound canvas size, duration, decoded image size and memory. Fail with a clear error, not a crash.
- Never execute anything from a package. The format contains no scripts, and version 0 has no field that can reference a URL or a file outside the package.

---

## 10. Not in version 0

Planned for later versions, not allowed now:

- 3D model layers, as glTF 2.0 with a camera, rendered into the image or video. Blender files are not planned directly; Blender exports glTF.
- transparent video, as input layers and as output
- keyframes on colours and text size
- transition presets beyond section 4.9, such as wipes
- colour management (ICC)
- vector (SVG) layers
- audio effects beyond gain and fades
- more than 2 audio channels
- a registered media type (IANA)

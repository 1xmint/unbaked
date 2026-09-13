# Unbaked format specification

**Version 0 — draft. Anything here may change.**

## 1. Carrier file

The outer file is a valid media file. Its normal content is the most recent
render.

| Finished result | Carrier | Extension |
|---|---|---|
| Still image | PNG | `.unbaked.png` |
| Sound only | MP4 audio | `.unbaked.m4a` |
| Moving picture, or picture with sound | MP4 | `.unbaked.mp4` |

## 2. Where the package lives

The package is a ZIP archive stored in a slot the carrier lets readers skip:

- PNG: an ancillary, private chunk. Chunk type to be fixed in this section.
- MP4 / M4A: a top-level `uuid` box with a fixed identifier to be fixed in this section.

Media files are stored in the ZIP uncompressed (STORED). Text files use DEFLATE.

## 3. Package contents

```
recipe.json   required
assets/       source files referenced by recipe.json
bake.json     required after the first render
```

## 4. recipe.json

To be written: timeline, tracks, clips, layer types (image, video, audio, text),
transforms, blend modes, effects, fonts, units and coordinate system.

## 5. bake.json

To be written: hash algorithm, what is hashed, render settings, renderer version.

## 6. Effects

Every effect is defined by exact math so independent renderers produce the
same output. To be written.

## 7. Versioning

`recipe.json` carries a `version` field. Readers reject versions they do not
understand rather than guessing.

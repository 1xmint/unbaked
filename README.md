# Unbaked

Media files that keep their layers.

An Unbaked file is a normal `.png`, `.m4a` or `.mp4`. Any photo viewer or media
player opens it and shows the finished result. Tucked inside, in a slot players
skip, is everything that made it: source images, clips, sound tracks, text,
fonts, effects, and a plain-text recipe describing how they combine.

Change the recipe, render again, and the finished result changes with it. A
person or an AI agent can receive a file, see it instantly, edit it, and send it
back.

```
poster.unbaked.png   still image
song.unbaked.m4a     sound
clip.unbaked.mp4     video, or picture + sound
```

## Status

Early design. Nothing here is usable yet. The format rules are being drafted in
[SPEC.md](SPEC.md).

## How it works

| Piece | What it does |
|---|---|
| Spec | The written rules: where the layers live, what each recipe field means, exactly how each effect is drawn |
| Renderer | Reads a recipe and its source files, produces the finished media |
| CLI | `pack`, `unpack`, `render`, `check` |

Inside every file:

```
recipe.json   tracks, layers, timing, position, text, fonts, effects, volume
assets/       the source files
bake.json     fingerprint of the recipe and assets at the last render
```

If someone edits a layer without rendering again, the fingerprint no longer
matches and tools report the preview as out of date.

## Known limits

- A normal player shows the last render. Live editing needs an Unbaked-aware viewer.
- Files hold both sources and the render, so they are roughly twice the size.
- Apps that re-compress uploads (Instagram, WhatsApp, Discord, X) strip the hidden layers.
- Video renders take real time: roughly the length of the clip or more on a small server.

## Prior art

OpenRaster (layers plus a required merged image), Photoshop PSD, Google Motion
Photos (a JPEG carrying an MP4), and C2PA Content Credentials (data in skip-able
PNG chunks and MP4 boxes).

## License

Code is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

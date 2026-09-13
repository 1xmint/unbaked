# Vision

Today a finished image or video is a dead end. The poster, the ad, the short
clip: once it is exported, the layers that made it are gone. To change one word
you need the original project file, the program that made it, the fonts, and
often the person who made it.

Unbaked keeps them together. A file is a normal picture, song or video that
opens anywhere, and it also carries everything needed to make it again with a
change.

## Built for machines first

Most media formats were designed for people clicking in an editor. Unbaked is
designed for software, and especially AI agents, to read and edit directly.
People benefit from that too, because anything an agent can do, a tool can
offer to a person.

What that means in practice:

- **The recipe is plain text.** An agent opens `recipe.json`, reads what is on
  screen and when, changes a field, and sends it back. No binary project file to
  decode, no editor to drive.
- **One way to say each thing.** Every field has one type. Times are whole
  milliseconds with the unit in the name (`start_ms`, `size_px`,
  `rotation_deg`), so there is no guessing between `1m` and `1ms`.
- **Mistakes are caught before rendering.** A published
  [JSON Schema](schema/recipe.schema.json) lets any tool tell an agent exactly
  which field is wrong. Unknown fields are errors, not silently ignored, so a
  typo never produces a quietly wrong picture.
- **The same file renders the same everywhere.** The rules are written down
  precisely enough that two renderers draw identical results. An agent can trust
  that what it asked for is what a person will see.
- **The file says when it is out of date.** A fingerprint records what was last
  rendered, so any tool can tell whether the picture still matches the recipe.
- **Nothing to fetch.** Everything the file needs is inside it, or named by an
  exact fingerprint. Rendering does not depend on the internet or on what happens
  to be installed.

## Where it is going

1. The written rules ([SPEC.md](SPEC.md)) and the schema.
2. A reference renderer in Rust that anyone can run: server, laptop, or browser.
3. A command-line tool to pack, unpack, render and check files.
4. A render service where an agent sends a file and an edited recipe and gets
   back the updated file, with nothing stored in between.
5. A viewer that shows edits live, before anything is rendered.

Later: 3D scenes (glTF), transparent video, and colour management.

## What it is not

- Not an editor. Editors can be built on it.
- Not a replacement for professional project files. It covers the common layer,
  text, effect and timeline work that most media needs, and keeps that small
  and exact.
- Not a guarantee that layers survive every app. Sites that re-compress uploads
  strip them. Direct transfers, cloud drives and APIs keep them.

# Recipe rule samples

Each file in `invalid/` passes `schema/recipe.schema.json` but breaks one rule
the schema cannot express (SPEC.md section 4, and the schema's `$comment`).
`tests/schema/check.sh` confirms they pass the schema, so each sample tests only
its rule.

`x-expect` names the JSON Pointer of the one problem a reader must report.
Readers ignore `x-` fields, so the field does not change the recipe.

The samples assume a package where:

- every path exists, except paths containing `missing`;
- a file's kind follows its extension: `.png` and `.jpg` are images, `.mp4` and
  `.m4a` are MP4, `.wav`, `.flac` and `.mp3` are audio, `.ttf` and `.otf` are
  fonts, and anything else is none of these.

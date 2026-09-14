# Fuzzing

Unbaked files come from strangers (SPEC.md section 9), so every reader is fuzzed
with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz). The
[Fuzz workflow](../.github/workflows/fuzz.yml) runs each target for 10 minutes
every day, and for 1 minute on pull requests that change this folder. A crash
fails the run and uploads the input that caused it as `crash-<target>`.

| Target | What it reads |
|---|---|
| `carrier` | PNG and MP4 slots: find, remove, and write then read back |
| `package` | The package ZIP: open, read every file, verify |
| `recipe` | `recipe.json` and the rules the schema cannot express |
| `bake` | `bake.json` |
| `open` | A whole file: detect, open, check |
| `media` | Asset files in the renderer: the MP4 reader, video frames, sound decoding |
| `scene` | Rendering a recipe before encoding, with small limits and fixed assets |

Starting inputs are in `seeds/` and the test fixtures under `tests/`.

To reproduce a crash on Linux or macOS with a nightly toolchain:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
cp Cargo.lock fuzz/Cargo.lock
cd fuzz
cargo fuzz run <target> path/to/crash-file
```

//! Changing a package: JSON Patch (RFC 6902) edits to `recipe.json`, and
//! adding asset files. Both check the result and change nothing unless it is
//! a valid recipe. Neither touches `bake.json`, so the file reads as stale
//! until it is rendered again.

use std::fmt;

use serde_json::{Map, Value};

use crate::json::{self, Problem, join};
use crate::pack::Files;
use crate::sniff::{self, AssetKind};
use crate::{bake, recipe, rules};

/// Why an edit was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// The patch is malformed or one of its operations failed. Paths point
    /// into the patch document, and messages name the recipe location.
    Patch(Vec<Problem>),
    /// The edited recipe breaks the spec. Paths point into the new recipe.
    Recipe(Vec<Problem>),
    /// The asset file or id cannot be used.
    Asset(String),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, problems) = match self {
            EditError::Asset(message) => return write!(f, "{message}"),
            EditError::Patch(problems) => ("the patch cannot be applied", problems),
            EditError::Recipe(problems) => ("the edited recipe.json is invalid", problems),
        };
        write!(f, "{what}:")?;
        for p in problems {
            write!(f, "\n  {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for EditError {}

/// Applies a JSON Patch to `recipe.json` and returns the changed files. The
/// new recipe is written as indented JSON with members in their existing order.
pub fn edit(files: &Files, patch: &[u8]) -> Result<Files, EditError> {
    let patch = json::parse(patch).map_err(|p| EditError::Patch(vec![p]))?;
    let mut doc = recipe_value(files)?;
    apply(&mut doc, &patch).map_err(EditError::Patch)?;
    with_recipe(files, &doc)
}

/// Stores `bytes` as `assets/<id>.<ext>` and points asset `id` at it. An
/// existing asset `id` keeps its other members, and its old file is removed if
/// nothing else uses it. `license` (section 4.12) is set when given; a packed
/// font needs one.
pub fn add_asset(
    files: &Files,
    id: &str,
    bytes: &[u8],
    license: Option<Value>,
) -> Result<Files, EditError> {
    if !recipe::is_id(id) {
        return Err(EditError::Asset(format!(
            "{id:?} is not a valid id (1-64 letters, digits, _ or -)"
        )));
    }
    let head = &bytes[..bytes.len().min(sniff::HEADER_LEN)];
    let ext = match sniff::detect(head) {
        Some(AssetKind::Png) => "png",
        Some(AssetKind::Jpeg) => "jpg",
        Some(AssetKind::Mp4) => "mp4",
        Some(AssetKind::Mp3) => "mp3",
        Some(AssetKind::Wav) => "wav",
        Some(AssetKind::Flac) => "flac",
        Some(AssetKind::Font) => "ttf",
        None => {
            return Err(EditError::Asset(
                "the file is not a supported image, video, sound or font (section 4.3)".into(),
            ));
        }
    };
    let mut doc = recipe_value(files)?;
    let Some(root) = doc.as_object_mut() else {
        return Err(EditError::Recipe(vec![Problem {
            path: String::new(),
            message: "must be an object".into(),
        }]));
    };
    let assets = root
        .entry("assets")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(assets) = assets.as_object_mut() else {
        return Err(EditError::Recipe(vec![Problem {
            path: "/assets".into(),
            message: "must be an object".into(),
        }]));
    };
    let entry = assets
        .entry(id)
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(entry) = entry.as_object_mut() else {
        return Err(EditError::Recipe(vec![Problem {
            path: join("/assets", id),
            message: "must be an object".into(),
        }]));
    };
    let old_path = entry.get("path").and_then(Value::as_str).map(str::to_owned);
    let new_path = format!("assets/{id}.{ext}");
    entry.shift_remove("ref");
    entry.insert("path".into(), new_path.clone().into());
    if let Some(license) = license {
        entry.insert("license".into(), license);
    }

    let mut changed = files.clone();
    changed.insert("recipe.json".into(), pretty(&doc));
    changed.insert(new_path.clone(), bytes.to_vec());
    if let Some(old) = old_path.filter(|old| *old != new_path) {
        // The old file may still be named by another asset or a licence.
        let parsed = recipe::parse(&changed["recipe.json"]).map_err(EditError::Recipe)?;
        if !bake::referenced_files(&parsed).contains(old.as_str()) {
            changed.remove(&old);
        }
    }
    validate(&changed)?;
    Ok(changed)
}

fn pretty(doc: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(doc).expect("JSON values serialise");
    bytes.push(b'\n');
    bytes
}

fn recipe_value(files: &Files) -> Result<Value, EditError> {
    let bytes = files.get("recipe.json").ok_or_else(|| {
        EditError::Recipe(vec![Problem {
            path: String::new(),
            message: "the package has no recipe.json".into(),
        }])
    })?;
    json::parse(bytes).map_err(|p| EditError::Recipe(vec![p]))
}

/// The files with `recipe.json` replaced by `doc`, if the result is valid.
fn with_recipe(files: &Files, doc: &Value) -> Result<Files, EditError> {
    let mut changed = files.clone();
    changed.insert("recipe.json".into(), pretty(doc));
    validate(&changed)?;
    Ok(changed)
}

fn validate(files: &Files) -> Result<(), EditError> {
    let parsed = recipe::parse(&files["recipe.json"]).map_err(EditError::Recipe)?;
    let kind_of = |path: &str| {
        files
            .get(path)
            .map(|d| sniff::detect(&d[..d.len().min(sniff::HEADER_LEN)]))
    };
    let problems = rules::check(&parsed, &kind_of);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(EditError::Recipe(problems))
    }
}

/// Applies an RFC 6902 patch to `doc`. Either every operation applies or
/// `doc` is left as it was.
pub fn apply(doc: &mut Value, patch: &Value) -> Result<(), Vec<Problem>> {
    let fail = |path: String, message: String| vec![Problem { path, message }];
    let Some(ops) = patch.as_array() else {
        return Err(fail(
            String::new(),
            "a patch must be an array of operations".into(),
        ));
    };
    let mut work = doc.clone();
    for (i, op) in ops.iter().enumerate() {
        let at = join("", &i.to_string());
        let Some(fields) = op.as_object() else {
            return Err(fail(at, "an operation must be an object".into()));
        };
        let text = |name: &str| match fields.get(name) {
            Some(Value::String(s)) => Ok(s.as_str()),
            Some(_) => Err(fail(join(&at, name), "must be a string".into())),
            None => Err(fail(join(&at, name), "is missing".into())),
        };
        let value = || {
            fields
                .get("value")
                .cloned()
                .ok_or_else(|| fail(join(&at, "value"), "is missing".into()))
        };
        let name = text("op")?;
        let path = text("path")?;
        let target = pointer(path)
            .ok_or_else(|| fail(join(&at, "path"), format!("{path:?} is not a JSON Pointer")))?;
        let from = || {
            let from = text("from")?;
            let tokens = pointer(from).ok_or_else(|| {
                fail(join(&at, "from"), format!("{from:?} is not a JSON Pointer"))
            })?;
            Ok::<_, Vec<Problem>>((from, tokens))
        };
        let result = match name {
            "add" => add(&mut work, &target, value()?),
            "remove" => remove(&mut work, &target).map(drop),
            "replace" => {
                let value = value()?;
                get_mut(&mut work, &target)
                    .map(|slot| *slot = value)
                    .ok_or_else(|| missing(path))
            }
            "move" => {
                let (from, source) = from()?;
                if target.len() > source.len() && target[..source.len()] == source[..] {
                    Err(format!("cannot move {from} into itself"))
                } else {
                    remove(&mut work, &source).and_then(|v| add(&mut work, &target, v))
                }
            }
            "copy" => {
                let (from, source) = from()?;
                match get(&work, &source).cloned() {
                    Some(v) => add(&mut work, &target, v),
                    None => Err(missing(from)),
                }
            }
            "test" => match get(&work, &target) {
                Some(v) if same(v, &value()?) => Ok(()),
                Some(_) => Err(format!("test failed: {path} has a different value")),
                None => Err(missing(path)),
            },
            other => {
                return Err(fail(
                    join(&at, "op"),
                    format!("{other:?} is not add, remove, replace, move, copy or test"),
                ));
            }
        };
        result.map_err(|message| fail(at.clone(), format!("{name} {path}: {message}")))?;
    }
    *doc = work;
    Ok(())
}

fn missing(path: &str) -> String {
    format!("{path} does not exist")
}

/// The reference tokens of a JSON Pointer (RFC 6901).
fn pointer(path: &str) -> Option<Vec<String>> {
    if path.is_empty() {
        return Some(Vec::new());
    }
    let rest = path.strip_prefix('/')?;
    rest.split('/')
        .map(|t| {
            let mut out = String::new();
            let mut chars = t.chars();
            while let Some(c) = chars.next() {
                out.push(match c {
                    '~' => match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return None,
                    },
                    c => c,
                });
            }
            Some(out)
        })
        .collect()
}

/// An array index token: digits, no leading zero, below `len` (or equal to it when `end` is allowed).
fn index(token: &str, len: usize, end: bool) -> Option<usize> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    let i: usize = token.parse().ok()?;
    (i < len || (end && i == len)).then_some(i)
}

fn get<'v>(doc: &'v Value, tokens: &[String]) -> Option<&'v Value> {
    tokens.iter().try_fold(doc, |v, t| match v {
        Value::Object(map) => map.get(t),
        Value::Array(items) => items.get(index(t, items.len(), false)?),
        _ => None,
    })
}

fn get_mut<'v>(doc: &'v mut Value, tokens: &[String]) -> Option<&'v mut Value> {
    tokens.iter().try_fold(doc, |v, t| match v {
        Value::Object(map) => map.get_mut(t),
        Value::Array(items) => {
            let i = index(t, items.len(), false)?;
            items.get_mut(i)
        }
        _ => None,
    })
}

fn add(doc: &mut Value, tokens: &[String], value: Value) -> Result<(), String> {
    let Some((last, parent)) = tokens.split_last() else {
        *doc = value;
        return Ok(());
    };
    let parent_path = || {
        parent
            .iter()
            .fold(String::new(), |path, token| join(&path, token))
    };
    match get_mut(doc, parent) {
        Some(Value::Object(map)) => {
            map.insert(last.clone(), value);
            Ok(())
        }
        Some(Value::Array(items)) => {
            let i = if last == "-" {
                items.len()
            } else {
                index(last, items.len(), true).ok_or_else(|| {
                    format!(
                        "{last:?} is not an index from 0 to {} or \"-\"",
                        items.len()
                    )
                })?
            };
            items.insert(i, value);
            Ok(())
        }
        Some(_) => Err(format!("{} is not an object or array", parent_path())),
        None => Err(missing(&parent_path())),
    }
}

fn remove(doc: &mut Value, tokens: &[String]) -> Result<Value, String> {
    let path = || tokens.iter().fold(String::new(), |p, t| join(&p, t));
    let Some((last, parent)) = tokens.split_last() else {
        return Err("cannot remove the whole document".into());
    };
    let removed = match get_mut(doc, parent) {
        Some(Value::Object(map)) => map.shift_remove(last),
        Some(Value::Array(items)) => index(last, items.len(), false).map(|i| items.remove(i)),
        _ => None,
    };
    removed.ok_or_else(|| missing(&path()))
}

/// JSON equality as RFC 6902 `test` defines it: numbers compare by value.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i128(), y.as_i128()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";
    const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0000000000";

    fn package() -> Files {
        let recipe = r#"{"unbaked": 0, "output": {"kind": "image", "width": 4, "height": 4},
            "assets": {"logo": {"path": "assets/logo.png"}},
            "layers": [{"id": "logo", "type": "image", "asset": "logo"}]}"#;
        Files::from([
            ("recipe.json".to_owned(), recipe.as_bytes().to_vec()),
            ("assets/logo.png".to_owned(), PNG.to_vec()),
        ])
    }

    fn recipe_of(files: &Files) -> Value {
        json::parse(&files["recipe.json"]).unwrap()
    }

    #[test]
    fn edits_keep_member_order_and_refuse_invalid_results() {
        let files = package();
        let patch = br#"[{"op": "replace", "path": "/output/width", "value": 8}]"#;
        let changed = edit(&files, patch).unwrap();
        let text = String::from_utf8(changed["recipe.json"].clone()).unwrap();
        assert!(
            text.starts_with("{\n  \"unbaked\": 0,\n  \"output\""),
            "{text}"
        );
        assert!(text.ends_with("}\n"));
        assert_eq!(recipe_of(&changed)["output"]["width"], 8);

        let patch = br#"[{"op": "remove", "path": "/assets/logo"}]"#;
        let EditError::Recipe(problems) = edit(&files, patch).unwrap_err() else {
            panic!("expected recipe problems");
        };
        assert_eq!(problems[0].path, "/layers/0/asset");

        let patch = br#"[{"op": "remove", "path": "/layers/3"}]"#;
        let err = edit(&files, patch).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the patch cannot be applied:\n  /0: remove /layers/3: /layers/3 does not exist"
        );
        assert!(matches!(
            edit(&files, b"[").unwrap_err(),
            EditError::Patch(_)
        ));
    }

    #[test]
    fn adding_assets() {
        let files = package();
        let added = add_asset(&files, "bg", JPEG, None).unwrap();
        assert_eq!(
            recipe_of(&added)["assets"]["bg"],
            json!({"path": "assets/bg.jpg"})
        );
        assert_eq!(added["assets/bg.jpg"], JPEG);
        assert_eq!(added.len(), 3);

        // Replacing an asset's file removes the old one.
        let swapped = add_asset(&files, "logo", JPEG, None).unwrap();
        assert_eq!(
            recipe_of(&swapped)["assets"]["logo"]["path"],
            "assets/logo.jpg"
        );
        assert!(!swapped.contains_key("assets/logo.png"));

        assert!(matches!(
            add_asset(&files, "bg", b"GIF89a", None),
            Err(EditError::Asset(_))
        ));
        assert!(matches!(
            add_asset(&files, "no/slash", PNG, None),
            Err(EditError::Asset(_))
        ));

        let font = b"\x00\x01\x00\x00000000000";
        let EditError::Recipe(problems) = add_asset(&files, "face", font, None).unwrap_err() else {
            panic!("a packed font needs a licence");
        };
        assert_eq!(problems[0].path, "/assets/face");
        let licence = json!({"spdx": "MIT"});
        let with_font = add_asset(&files, "face", font, Some(licence.clone())).unwrap();
        assert_eq!(recipe_of(&with_font)["assets"]["face"]["license"], licence);
    }

    fn patched(doc: Value, patch: Value) -> Result<Value, Vec<Problem>> {
        let mut doc = doc;
        apply(&mut doc, &patch).map(|()| doc)
    }

    #[test]
    fn every_operation() {
        let doc = json!({"a": [1, 2], "b": {"c": "x"}});
        let ops = |ops| patched(doc.clone(), ops).unwrap();
        assert_eq!(
            ops(json!([{"op": "add", "path": "/a/1", "value": 9}])),
            json!({"a": [1, 9, 2], "b": {"c": "x"}})
        );
        assert_eq!(
            ops(
                json!([{"op": "add", "path": "/a/-", "value": 3}, {"op": "add", "path": "/d", "value": null}])
            ),
            json!({"a": [1, 2, 3], "b": {"c": "x"}, "d": null})
        );
        assert_eq!(
            ops(json!([{"op": "remove", "path": "/a/0"}])),
            json!({"a": [2], "b": {"c": "x"}})
        );
        assert_eq!(
            ops(json!([{"op": "replace", "path": "/b/c", "value": "y"}])),
            json!({"a": [1, 2], "b": {"c": "y"}})
        );
        assert_eq!(
            ops(json!([{"op": "move", "from": "/b/c", "path": "/a/0"}])),
            json!({"a": ["x", 1, 2], "b": {}})
        );
        assert_eq!(
            ops(json!([{"op": "copy", "from": "/a", "path": "/b/a"}])),
            json!({"a": [1, 2], "b": {"c": "x", "a": [1, 2]}})
        );
        assert_eq!(
            ops(json!([{"op": "test", "path": "/a/1", "value": 2.0}])),
            doc.clone()
        );
        assert_eq!(
            ops(json!([{"op": "replace", "path": "", "value": [true]}])),
            json!([true])
        );
        // Escaped tokens and unknown members.
        let doc = json!({"a/b": {"~": 1}});
        assert_eq!(
            patched(
                doc,
                json!([{"op": "replace", "path": "/a~1b/~0", "value": 2, "note": "x"}])
            )
            .unwrap(),
            json!({"a/b": {"~": 2}})
        );
    }

    #[test]
    fn a_failing_operation_changes_nothing_and_names_itself() {
        let mut doc = json!({"a": [1]});
        let patch = json!([
            {"op": "add", "path": "/a/-", "value": 2},
            {"op": "test", "path": "/a/0", "value": 5},
        ]);
        let problems = apply(&mut doc, &patch).unwrap_err();
        assert_eq!(doc, json!({"a": [1]}));
        assert_eq!(problems[0].path, "/1");
        assert_eq!(
            problems[0].message,
            "test /a/0: test failed: /a/0 has a different value"
        );

        let err = |patch| patched(json!({"a": [1], "s": "t"}), patch).unwrap_err()[0].clone();
        let p = err(json!([{"op": "remove", "path": "/a/01"}]));
        assert_eq!(
            (p.path.as_str(), p.message.as_str()),
            ("/0", "remove /a/01: /a/01 does not exist")
        );
        assert_eq!(
            err(json!([{"op": "replace", "path": "/x"}])).path,
            "/0/value"
        );
        assert_eq!(
            err(json!([{"op": "add", "path": "/a/5", "value": 1}])).path,
            "/0"
        );
        assert_eq!(
            err(json!([{"op": "add", "path": "/s/t", "value": 1}])).message,
            "add /s/t: /s is not an object or array"
        );
        assert_eq!(
            err(json!([{"op": "move", "from": "/a", "path": "/a/0"}])).message,
            "move /a/0: cannot move /a into itself"
        );
        assert_eq!(err(json!([{"op": "copy", "path": "/b"}])).path, "/0/from");
        assert_eq!(err(json!([{"op": "jump", "path": ""}])).path, "/0/op");
        assert_eq!(
            err(json!([{"op": "add", "path": "a", "value": 1}])).path,
            "/0/path"
        );
        assert_eq!(err(json!({"op": "add"})).path, "");
    }
}

//! `bake.json`, SPEC.md section 7: the fingerprints of the last render, and the
//! comparison that says whether the render is still up to date.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::json::{self, Problem, Problems, join};

/// A parsed `bake.json`. Hashes are 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bake {
    pub renderer: String,
    pub recipe_sha256: String,
    /// Package path -> hash, for every file an asset's `path` or `license.file` names.
    pub assets_sha256: BTreeMap<String, String>,
    pub render_sha256: String,
}

/// Reads `bake.json`, checking its structure.
pub fn parse(bytes: &[u8]) -> Result<Bake, Vec<Problem>> {
    let value = json::parse(bytes).map_err(|p| vec![p])?;
    let mut p = Problems::default();
    match bake(&mut p, &value) {
        Some(b) if p.is_empty() => Ok(b),
        _ => Err(p.into_vec()),
    }
}

fn hash(p: &mut Problems, v: &Value, path: &str) -> Option<String> {
    let s = p.string(v, path)?;
    if json::is_sha256_hex(s) {
        Some(s.to_owned())
    } else {
        p.add(path, "must be 64 lowercase hex characters");
        None
    }
}

fn bake(p: &mut Problems, v: &Value) -> Option<Bake> {
    const FIELDS: &[&str] = &[
        "unbaked",
        "renderer",
        "recipe_sha256",
        "assets_sha256",
        "render_sha256",
    ];
    let map = p.object(v, "", FIELDS)?;
    let mut field = |key: &str| {
        let v = p.required(map, "", key);
        v.map(|v| (v, join("", key)))
    };
    let version = field("unbaked");
    let renderer = field("renderer");
    let recipe = field("recipe_sha256");
    let assets = field("assets_sha256");
    let render = field("render_sha256");

    if let Some((v, path)) = version
        && let Some(n) = p.integer(v, &path, 0)
        && n != u64::from(crate::SPEC_VERSION)
    {
        p.add(
            &path,
            format!(
                "spec version {n} is not supported; this reader implements version {}",
                crate::SPEC_VERSION
            ),
        );
    }
    let renderer = renderer.and_then(|(v, path)| p.string(v, &path).map(str::to_owned));
    let recipe_sha256 = recipe.and_then(|(v, path)| hash(p, v, &path));
    let render_sha256 = render.and_then(|(v, path)| hash(p, v, &path));
    let assets_sha256 = assets.and_then(|(v, path)| {
        let Some(entries) = v.as_object() else {
            p.add(
                &path,
                format!("expected an object, found {}", json::kind(v)),
            );
            return None;
        };
        let mut out = BTreeMap::new();
        for (name, h) in entries {
            let entry_path = join(&path, name);
            let name_ok = crate::recipe::package_path(p, &Value::String(name.clone()), &entry_path);
            if let (Some(name), Some(h)) = (name_ok, hash(p, h, &entry_path)) {
                out.insert(name, h);
            }
        }
        Some(out)
    });
    Some(Bake {
        renderer: renderer?,
        recipe_sha256: recipe_sha256?,
        assets_sha256: assets_sha256?,
        render_sha256: render_sha256?,
    })
}

/// Whether the render still matches the recipe and assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Every fingerprint matches.
    Fresh,
    /// The recipe or assets changed since the render. Lists what changed.
    Stale(Vec<Change>),
    /// Recipe and assets match, but the visible media was edited outside Unbaked.
    RenderModified,
}

/// One reason a render is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Recipe,
    AssetChanged(String),
    /// A file the recipe now references that the render did not use.
    AssetAdded(String),
    /// A file the render used that the recipe no longer references.
    AssetRemoved(String),
    /// The recipe's `output.kind` needs a different carrier than the file is.
    Carrier,
}

/// Hashes of what the file holds now, to compare with `bake.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current<'a> {
    pub recipe_sha256: &'a str,
    /// Package path -> hash, for every file the recipe references.
    pub assets_sha256: BTreeMap<&'a str, &'a str>,
    pub render_sha256: &'a str,
    /// The carrier fits the recipe's `output.kind`.
    pub carrier_fits: bool,
}

/// Compares the file as it is now with its `bake.json`. Stale wins over render-modified.
pub fn compare(bake: &Bake, now: &Current) -> Freshness {
    let mut changes = Vec::new();
    if bake.recipe_sha256 != now.recipe_sha256 {
        changes.push(Change::Recipe);
    }
    if !now.carrier_fits {
        changes.push(Change::Carrier);
    }
    for (&path, &h) in &now.assets_sha256 {
        match bake.assets_sha256.get(path) {
            None => changes.push(Change::AssetAdded(path.to_owned())),
            Some(old) if old != h => changes.push(Change::AssetChanged(path.to_owned())),
            Some(_) => {}
        }
    }
    for path in bake.assets_sha256.keys() {
        if !now.assets_sha256.contains_key(path.as_str()) {
            changes.push(Change::AssetRemoved(path.clone()));
        }
    }
    if !changes.is_empty() {
        Freshness::Stale(changes)
    } else if bake.render_sha256 != now.render_sha256 {
        Freshness::RenderModified
    } else {
        Freshness::Fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn sample() -> String {
        format!(
            r#"{{"unbaked": 0, "renderer": "test 1", "recipe_sha256": "{A}",
                "assets_sha256": {{"assets/a.png": "{A}"}}, "render_sha256": "{A}",
                "x-note": "ignored"}}"#
        )
    }

    #[test]
    fn valid_bake_parses() {
        let bake = parse(sample().as_bytes()).unwrap();
        assert_eq!(bake.renderer, "test 1");
        assert_eq!(bake.assets_sha256["assets/a.png"], A);
    }

    #[test]
    fn bad_bakes_report_where() {
        let cases = [
            (
                sample().replace("\"unbaked\": 0", "\"unbaked\": 1"),
                "/unbaked",
            ),
            (sample().replace("\"x-note\"", "\"note\""), "/note"),
            (sample().replacen(A, "ABC", 1), "/recipe_sha256"),
            (
                sample().replace("assets/a.png", "../a.png"),
                "/assets_sha256/..~1a.png",
            ),
            (sample().replace("\"renderer\": \"test 1\",", ""), ""),
        ];
        for (text, path) in cases {
            let problems = parse(text.as_bytes()).unwrap_err();
            assert_eq!(problems[0].path, path, "{problems:?}");
        }
    }

    fn now(recipe: &'static str, assets: &[(&'static str, &'static str)]) -> Current<'static> {
        Current {
            recipe_sha256: recipe,
            assets_sha256: assets.iter().copied().collect(),
            render_sha256: A,
            carrier_fits: true,
        }
    }

    #[test]
    fn comparison_reports_each_result() {
        let bake = parse(sample().as_bytes()).unwrap();
        assert_eq!(
            compare(&bake, &now(A, &[("assets/a.png", A)])),
            Freshness::Fresh
        );
        assert_eq!(
            compare(
                &bake,
                &now(B, &[("assets/a.png", B), ("assets/new.png", A)])
            ),
            Freshness::Stale(vec![
                Change::Recipe,
                Change::AssetChanged("assets/a.png".into()),
                Change::AssetAdded("assets/new.png".into()),
            ])
        );
        assert_eq!(
            compare(&bake, &now(A, &[])),
            Freshness::Stale(vec![Change::AssetRemoved("assets/a.png".into())])
        );
        let mut edited = now(A, &[("assets/a.png", A)]);
        edited.render_sha256 = B;
        assert_eq!(compare(&bake, &edited), Freshness::RenderModified);
        edited.carrier_fits = false;
        assert_eq!(
            compare(&bake, &edited),
            Freshness::Stale(vec![Change::Carrier])
        );
    }
}

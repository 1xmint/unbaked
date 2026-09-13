//! Strict JSON loading for `recipe.json` and `bake.json`, plus the helpers both
//! readers use to walk a document and report problems by JSON Pointer.

use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Most problems reported for one document. Past this, one final problem says more were cut.
pub const MAX_PROBLEMS: usize = 100;

/// One thing wrong with a JSON document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// JSON Pointer (RFC 6901) to the value, `""` for the whole document.
    pub path: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Parses JSON, rejecting an object that repeats a key. Serde would otherwise keep
/// the last copy silently, and another reader might keep the first.
pub fn parse(bytes: &[u8]) -> Result<Value, Problem> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Strict
        .deserialize(&mut de)
        .and_then(|v| de.end().map(|()| v))
        .map_err(|e| Problem {
            path: String::new(),
            message: format!("not valid JSON: {e}"),
        })?;
    Ok(value)
}

struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("number out of range"))
    }
    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }
    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut out = Vec::new();
        while let Some(v) = seq.next_element_seed(Strict)? {
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format!("key {key:?} appears twice")));
            }
            let value = map.next_value_seed(Strict)?;
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }
}

/// Appends one reference token to a JSON Pointer, escaping `~` and `/`.
pub fn join(path: &str, token: &str) -> String {
    format!("{path}/{}", token.replace('~', "~0").replace('/', "~1"))
}

/// Collects problems while a document is walked.
#[derive(Debug, Default)]
pub struct Problems {
    list: Vec<Problem>,
    cut: bool,
}

impl Problems {
    pub fn add(&mut self, path: &str, message: impl Into<String>) {
        if self.list.len() < MAX_PROBLEMS {
            self.list.push(Problem {
                path: path.to_owned(),
                message: message.into(),
            });
        } else {
            self.cut = true;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn into_vec(mut self) -> Vec<Problem> {
        if self.cut {
            self.list.push(Problem {
                path: String::new(),
                message: format!("more than {MAX_PROBLEMS} problems; the rest are not listed"),
            });
        }
        self.list
    }

    /// The value as an object whose keys are all in `allowed` or start with `x-`.
    pub fn object<'v>(
        &mut self,
        v: &'v Value,
        path: &str,
        allowed: &[&str],
    ) -> Option<&'v Map<String, Value>> {
        let Some(map) = v.as_object() else {
            self.add(path, format!("expected an object, found {}", kind(v)));
            return None;
        };
        for key in map.keys() {
            if !key.starts_with("x-") && !allowed.contains(&key.as_str()) {
                self.add(&join(path, key), "unknown field");
            }
        }
        Some(map)
    }

    /// A required field. Reports it missing and returns `None` when absent.
    pub fn required<'v>(
        &mut self,
        map: &'v Map<String, Value>,
        path: &str,
        key: &str,
    ) -> Option<&'v Value> {
        let v = map.get(key);
        if v.is_none() {
            self.add(path, format!("missing required field {key:?}"));
        }
        v
    }

    pub fn number(&mut self, v: &Value, path: &str) -> Option<f64> {
        let n = v.as_f64();
        if n.is_none() {
            self.add(path, format!("expected a number, found {}", kind(v)));
        }
        n
    }

    /// A whole number `>= min`. `1.0` counts as whole, as in JSON Schema.
    pub fn integer(&mut self, v: &Value, path: &str, min: u64) -> Option<u64> {
        let whole = v.as_f64().is_some_and(|f| f.fract() == 0.0);
        if !whole {
            self.add(path, format!("expected a whole number, found {}", kind(v)));
            return None;
        }
        let n = v.as_u64().or_else(|| {
            v.as_f64()
                .filter(|f| *f >= 0.0 && *f < u64::MAX as f64)
                .map(|f| f as u64)
        });
        match n {
            Some(n) if n >= min => Some(n),
            None if v.as_f64().is_some_and(|f| f > 0.0) => {
                self.add(path, "is too large");
                None
            }
            _ => {
                self.add(path, format!("must be at least {min}"));
                None
            }
        }
    }

    pub fn string<'v>(&mut self, v: &'v Value, path: &str) -> Option<&'v str> {
        let s = v.as_str();
        if s.is_none() {
            self.add(path, format!("expected a string, found {}", kind(v)));
        }
        s
    }

    pub fn boolean(&mut self, v: &Value, path: &str) -> Option<bool> {
        let b = v.as_bool();
        if b.is_none() {
            self.add(path, format!("expected true or false, found {}", kind(v)));
        }
        b
    }

    /// A string that must be one of `choices`.
    pub fn choice<T: Copy>(&mut self, v: &Value, path: &str, choices: &[(&str, T)]) -> Option<T> {
        let s = self.string(v, path)?;
        let found = choices.iter().find(|(name, _)| *name == s).map(|(_, t)| *t);
        if found.is_none() {
            let names: Vec<_> = choices.iter().map(|(n, _)| format!("{n:?}")).collect();
            self.add(path, format!("{s:?} is not one of {}", names.join(", ")));
        }
        found
    }
}

/// A short name for a JSON value's type, for messages.
pub fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(n) if n.is_f64() => "a number with a fraction",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// True for exactly 64 lowercase hex characters.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_keys_are_rejected_at_any_depth() {
        assert!(parse(br#"{"a": 1, "a": 2}"#).is_err());
        let nested = parse(br#"{"a": [{"b": 1, "b": 1}]}"#).unwrap_err();
        assert!(nested.message.contains("\"b\" appears twice"), "{nested}");
        assert!(parse(br#"{"a": {"b": 1}, "b": {"a": 1}}"#).is_ok());
    }

    #[test]
    fn trailing_data_and_bad_utf8_are_rejected() {
        assert!(parse(b"{} {}").is_err());
        assert!(parse(b"\"\xff\"").is_err());
        assert!(parse(b"\xef\xbb\xbf{}").is_err());
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let deep = "[".repeat(10_000) + &"]".repeat(10_000);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn pointer_tokens_are_escaped() {
        assert_eq!(join("/assets", "a/b~c"), "/assets/a~1b~0c");
    }

    #[test]
    fn whole_numbers_follow_json_schema() {
        let mut p = Problems::default();
        assert_eq!(p.integer(&serde_json::json!(5), "", 0), Some(5));
        assert_eq!(p.integer(&serde_json::json!(5.0), "", 0), Some(5));
        assert!(p.is_empty());
        assert_eq!(p.integer(&serde_json::json!(5.5), "/a", 0), None);
        assert_eq!(p.integer(&serde_json::json!(-5), "/b", 0), None);
        assert_eq!(p.integer(&serde_json::json!(0), "/c", 1), None);
        let msgs: Vec<_> = p.into_vec().into_iter().map(|p| p.to_string()).collect();
        assert_eq!(
            msgs,
            [
                "/a: expected a whole number, found a number with a fraction",
                "/b: must be at least 0",
                "/c: must be at least 1",
            ]
        );
    }

    #[test]
    fn problems_are_capped() {
        let mut p = Problems::default();
        for _ in 0..150 {
            p.add("", "x");
        }
        let list = p.into_vec();
        assert_eq!(list.len(), MAX_PROBLEMS + 1);
        assert!(list[MAX_PROBLEMS].message.contains("more than 100"));
    }
}

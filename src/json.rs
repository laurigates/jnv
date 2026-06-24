use jaq_core::{
    load::{Arena, File, Loader},
    Compiler, Ctx, RcIter,
};
use jaq_json::Val;

use promkit_widgets::{
    jsonstream::jsonz,
    serde_json::{self, Deserializer, Value},
};

/// An error from running a jq filter, split by phase so callers can treat an
/// incomplete or syntactically invalid expression — common while the user is
/// still typing — differently from a genuine runtime failure.
#[derive(Debug)]
pub enum JaqError {
    /// The filter could not be parsed or compiled. This is the expected state
    /// for a half-typed expression (e.g. a trailing `|`), so callers typically
    /// stay quiet rather than surfacing it as an error.
    Invalid(String),
    /// The filter parsed and compiled but failed during execution. This is a
    /// real error worth reporting, since the filter itself is well-formed.
    Execution(String),
}

impl std::fmt::Display for JaqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JaqError::Invalid(msg) | JaqError::Execution(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for JaqError {}

/// Get all JSON paths from the input JSON string,
/// respecting the max_streams limit if provided.
pub async fn get_all_paths(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<impl Iterator<Item = String>> {
    let stream = deserialize(json_str, max_streams)?;
    Ok(paths_of(&stream).into_iter())
}

/// Enumerate all JSON paths reachable in the given parsed values.
pub fn paths_of(values: &[serde_json::Value]) -> Vec<String> {
    jsonz::get_all_paths(values.iter()).collect()
}

/// Deserialize JSON string into a vector of serde_json::Value.
/// If max_streams is given, only deserialize up to that many JSON values.
pub fn deserialize(
    json_str: &str,
    max_streams: Option<usize>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let deserializer: serde_json::StreamDeserializer<'_, serde_json::de::StrRead<'_>, Value> =
        Deserializer::from_str(json_str).into_iter::<serde_json::Value>();
    let results = match max_streams {
        Some(l) => deserializer.take(l).collect::<Result<Vec<_>, _>>(),
        None => deserializer.collect::<Result<Vec<_>, _>>(),
    };
    results.map_err(anyhow::Error::from)
}

pub fn run_jaq(
    query: &str,
    json_stream: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, JaqError> {
    let arena = Arena::default();
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let modules = loader
        .load(
            &arena,
            File {
                code: query,
                path: (),
            },
        )
        // Parse failures are usually just an incomplete expression mid-typing.
        .map_err(|_| JaqError::Invalid("incomplete or invalid jq filter".to_string()))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|_| JaqError::Invalid("invalid jq filter".to_string()))?;

    let mut ret = Vec::<serde_json::Value>::new();

    for input in json_stream {
        let inputs = RcIter::new(core::iter::empty());
        let out = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));
        for item in out {
            match item {
                Ok(val) => ret.push(val.into()),
                Err(err) => return Err(JaqError::Execution(err.to_string())),
            }
        }
    }

    Ok(ret)
}

/// Evaluate `base` against the already-parsed input stream, then enumerate the
/// JSON paths of the result. Used for context-aware completion after a pipe:
/// suggestions are relative to the output of the base expression rather than the
/// root document. Takes a pre-parsed stream so callers can cache deserialization.
pub fn relative_paths(base: &str, stream: &[serde_json::Value]) -> anyhow::Result<Vec<String>> {
    let intermediate = run_jaq(base, stream)?;
    Ok(paths_of(&intermediate))
}

/// Summarize a jq result stream as a short `type · length` string, mirroring
/// what `| type` and `| length` would report. Used for a status-line hint so
/// users don't have to append those filters manually.
///
/// A single value is described by its type and (where meaningful) its length;
/// a multi-value stream is reported as a count.
pub fn summarize(values: &[serde_json::Value]) -> String {
    match values {
        [] => "empty (0 results)".to_string(),
        [value] => summarize_value(value),
        many => format!("stream · {} values", many.len()),
    }
}

fn summarize_value(value: &serde_json::Value) -> String {
    match value {
        Value::Object(map) => format!("object · {} {}", map.len(), pluralize(map.len(), "key")),
        Value::Array(items) => {
            format!("array · {} {}", items.len(), pluralize(items.len(), "item"))
        }
        Value::String(s) => {
            let count = s.chars().count();
            format!("string · {} {}", count, pluralize(count, "char"))
        }
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Null => "null".to_string(),
    }
}

fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use promkit_widgets::serde_json::json;

    fn parse(s: &str) -> Vec<serde_json::Value> {
        deserialize(s, None).unwrap()
    }

    #[test]
    fn root_paths_use_dot_and_index_notation() {
        let mut p = paths_of(&parse(r#"{"foo": {"bar": 1}, "qux": [10, 20]}"#));
        p.sort();
        assert_eq!(p, [".", ".foo", ".foo.bar", ".qux", ".qux[0]", ".qux[1]"]);
    }

    #[test]
    fn relative_paths_are_relative_to_base_output() {
        let stream = parse(r#"{"foo": {"bar": 1, "baz": 2}, "qux": 9}"#);
        let mut p = relative_paths(".foo", &stream).unwrap();
        p.sort();
        // Keys of `.foo`, not of the root document (no `.qux`).
        assert_eq!(p, [".", ".bar", ".baz"]);
    }

    #[test]
    fn relative_paths_descend_into_array_elements() {
        // `.items[] | .` — the element-relative case (map/select interiors).
        let stream = parse(r#"{"items": [{"name": "a", "qty": 1}]}"#);
        let mut p = relative_paths(".items[]", &stream).unwrap();
        p.sort();
        assert_eq!(p, [".", ".name", ".qty"]);
    }

    #[test]
    fn relative_paths_errors_on_invalid_base() {
        let stream = parse(r#"{"foo": 1}"#);
        // An incomplete/invalid base expression must surface as an error so the
        // caller can fall back to offering no suggestions rather than crashing.
        assert!(relative_paths("this is not jq (", &stream).is_err());
    }

    #[test]
    fn summarize_object_array_string() {
        assert_eq!(summarize(&[json!({"a": 1, "b": 2})]), "object · 2 keys");
        assert_eq!(summarize(&[json!({"a": 1})]), "object · 1 key");
        assert_eq!(summarize(&[json!([1, 2, 3])]), "array · 3 items");
        assert_eq!(summarize(&[json!([1])]), "array · 1 item");
        assert_eq!(summarize(&[json!("hello")]), "string · 5 chars");
    }

    #[test]
    fn summarize_scalars() {
        assert_eq!(summarize(&[json!(42)]), "number");
        assert_eq!(summarize(&[json!(true)]), "boolean");
        assert_eq!(summarize(&[json!(null)]), "null");
    }

    #[test]
    fn summarize_stream_and_empty() {
        assert_eq!(summarize(&[json!(1), json!(2)]), "stream · 2 values");
        assert_eq!(summarize(&[]), "empty (0 results)");
    }

    #[test]
    fn summarize_string_counts_unicode_scalar_values() {
        // "café" is 4 chars even though 'é' is multi-byte.
        assert_eq!(summarize(&[json!("café")]), "string · 4 chars");
    }
}

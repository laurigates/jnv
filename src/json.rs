use jaq_core::{
    load::{Arena, File, Loader},
    Compiler, Ctx, RcIter,
};
use jaq_json::Val;

use promkit_widgets::{
    jsonstream::jsonz,
    serde_json::{self, Deserializer, Value},
};

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
) -> anyhow::Result<Vec<serde_json::Value>> {
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
        .map_err(|errs| anyhow::anyhow!("jq filter parsing failed: {errs:?}"))?;
    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| anyhow::anyhow!("jq filter compilation failed: {errs:?}"))?;

    let mut ret = Vec::<serde_json::Value>::new();

    for input in json_stream {
        let inputs = RcIter::new(core::iter::empty());
        let out = filter.run((Ctx::new([], &inputs), Val::from(input.clone())));
        for item in out {
            match item {
                Ok(val) => ret.push(val.into()),
                Err(err) => return Err(anyhow::anyhow!("jq filter execution failed: {err}")),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

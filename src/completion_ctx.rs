//! Prototype: determine the completion context within a jq query.
//!
//! Splits a query into (base_expr, segment) at the last *top-level* pipe,
//! ignoring pipes inside string literals and parentheses. The `base_expr`
//! is fed to run_jaq; the `segment` is the trailing path being completed.
//!
//! Segmentation uses jaq's own tokenizer (`jaq_core::load::lex`) rather than a
//! hand-rolled byte scanner, so it can never disagree with how jq parses the
//! query. The lexer already handles string literals/escapes/interpolation
//! (`Tok::Str`) and groups each `(...)`/`[...]`/`{...}` into one recursive
//! `Tok::Block`; a `|` nested inside any of those is simply not a top-level
//! token. The `|=` update-assignment lexes as a distinct `Tok::Sym` (`"|="`),
//! so excluding it is free.

use jaq_core::load::lex::{Lexer, Tok};

#[derive(Debug, PartialEq, Eq)]
pub struct CompletionCtx {
    /// Expression before the last top-level pipe (to evaluate). Empty => root.
    pub base: String,
    /// Byte offset where the segment-to-complete begins (after pipe + whitespace).
    pub segment_start: usize,
}

/// Find the byte offset of the last top-level `|` pipe symbol (not `|=`),
/// using jaq's lexer.
///
/// Returns `None` when there is no top-level pipe, or when the query fails to
/// lex (e.g. unbalanced delimiters during mid-typing like `select(.a`); callers
/// then degrade to the root context. This is never worse than the old byte
/// scanner, which "succeeded" on such input but produced an invalid `base` that
/// downstream evaluation rejected into empty suggestions anyway.
fn last_top_level_pipe(query: &str) -> Option<usize> {
    // Walk only the top-level token vec; never descend into `Tok::Block`.
    let tokens = Lexer::new(query).lex().ok()?;
    let last = tokens
        .iter()
        .rfind(|tok| matches!(tok.1, Tok::Sym) && tok.0 == "|")?;
    // Recover the slice's byte offset within `query` (the same computation as
    // jaq's private `load::span`, inlined to avoid depending on its visibility).
    Some(last.0.as_ptr() as usize - query.as_ptr() as usize)
}

pub fn analyze(query: &str) -> CompletionCtx {
    match last_top_level_pipe(query) {
        Some(p) => {
            let base = query[..p].trim().to_string();
            // segment begins after the pipe, skipping whitespace
            let after = p + 1;
            let ws = query[after..].len() - query[after..].trim_start().len();
            CompletionCtx {
                base,
                segment_start: after + ws,
            }
        }
        None => {
            // no pipe: whole query is the segment (skip leading ws)
            let ws = query.len() - query.trim_start().len();
            CompletionCtx {
                base: String::new(),
                segment_start: ws,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg<'a>(q: &'a str, ctx: &CompletionCtx) -> &'a str {
        &q[ctx.segment_start..]
    }

    #[test]
    fn table() {
        let cases = [
            // (query, expected base, expected segment)
            (".foo | .ba", ".foo", ".ba"),
            (".foo|.ba", ".foo", ".ba"),
            (".", "", "."),
            (".foo.ba", "", ".foo.ba"),
            (".a | .b | .c", ".a | .b", ".c"),
            ("(.a | .b) | .c", "(.a | .b)", ".c"),
            (".items[] | .na", ".items[]", ".na"),
            (r#".foo["a|b"] | .c"#, r#".foo["a|b"]"#, ".c"),
            ("{a: .x | .y} | .z", "{a: .x | .y}", ".z"), // pipe inside object value
            (".foo | ", ".foo", ""),
            (".foo |= .bar", "", ".foo |= .bar"), // |= not a boundary
            ("  .foo", "", ".foo"),               // leading ws
            ("map(.a) | .b", "map(.a)", ".b"),
            // token-tree specifics:
            (".a.", "", ".a."), // open trailing dot lexes cleanly (`.a`, `.`)
            ("select(.a", "", "select(.a"), // unbalanced: degrades to root, no panic
            (r#""\(.a|.b)" | .c"#, r#""\(.a|.b)""#, ".c"), // pipe inside string interpolation
        ];
        let mut failures = vec![];
        for (q, want_base, want_seg) in cases {
            let ctx = analyze(q);
            let got_seg = seg(q, &ctx);
            if ctx.base != want_base || got_seg != want_seg {
                failures.push(format!(
                    "query {q:?}: got base={:?} seg={:?}, want base={:?} seg={:?}",
                    ctx.base, got_seg, want_base, want_seg
                ));
            }
        }
        for f in &failures {
            eprintln!("FAIL: {f}");
        }
        assert!(
            failures.is_empty(),
            "{} segmentation cases failed",
            failures.len()
        );
    }
}

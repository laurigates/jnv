//! Prototype: determine the completion context within a jq query.
//!
//! Splits a query into (base_expr, segment) at the last *top-level* pipe,
//! ignoring pipes inside string literals and parentheses. The `base_expr`
//! is fed to run_jaq; the `segment` is the trailing path being completed.

#[derive(Debug, PartialEq, Eq)]
pub struct CompletionCtx {
    /// Expression before the last top-level pipe (to evaluate). Empty => root.
    pub base: String,
    /// Byte offset where the segment-to-complete begins (after pipe + whitespace).
    pub segment_start: usize,
}

/// Find the byte index of the last top-level `|` that is a pipe (not `|=`),
/// skipping string literals and any `(...)`/`[...]`/`{...}` nesting.
fn last_top_level_pipe(q: &str) -> Option<usize> {
    let bytes = q.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut last_pipe = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'|' if depth == 0 => {
                // `|=` is update-assignment, not a pipe boundary we complete after.
                let next_eq = bytes.get(i + 1) == Some(&b'=');
                if !next_eq {
                    last_pipe = Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    last_pipe
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

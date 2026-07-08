//! pxp — a fast PHP superset transpiler.
//!
//! Architecture:
//!   source ──lex──▶ tokens+spans ──transform passes──▶ edits ──splice──▶ PHP
//!
//! The guiding principles:
//!   * **Span-splice, don't pretty-print.** Untouched code is copied verbatim, so
//!     formatting/comments are preserved and only changed regions cost anything.
//!   * **Line-count-preserving desugarings.** No source maps needed — line N in
//!     the source is line N in the output.

pub mod ast;
pub mod astcount;
pub mod bytestr;
pub mod emit;
pub mod lexer;
pub mod line_index;
pub mod parser;
pub mod scope;
pub mod sourcemap;
pub mod span;
pub mod token;
pub mod transform;

use sourcemap::SourceMap;

/// Transpile pxp source (raw bytes — PHP source isn't guaranteed UTF-8) into
/// plain PHP bytes.
pub fn transpile(src: impl AsRef<[u8]>) -> Vec<u8> {
    let src = src.as_ref();
    let edits = transform::run_all(src);
    emit::apply(src, edits).0
}

/// Convenience for callers that hold UTF-8 (tests, quick tooling). The output is
/// decoded lossily — use [`transpile`] for byte-exact results.
pub fn transpile_str(src: &str) -> String {
    String::from_utf8_lossy(&transpile(src.as_bytes())).into_owned()
}

/// Transpile and also produce a [`SourceMap`] relating the generated PHP back to
/// the source, for translating exception/stack-trace lines.
pub fn transpile_with_map(
    src: impl AsRef<[u8]>,
    source_path: impl Into<String>,
    generated_path: impl Into<String>,
) -> (Vec<u8>, SourceMap) {
    let src = src.as_ref();
    let edits = transform::run_all(src);
    let (generated, segments) = emit::apply(src, edits);
    let map = SourceMap::from_segments(source_path, generated_path, src, &generated, &segments);
    (generated, map)
}

#[cfg(test)]
mod tests {
    use super::transpile;

    // Cross-cutting invariant of the whole pipeline: plain PHP round-trips
    // byte-for-byte. Feature-specific tests live alongside their transform
    // (e.g. `transform::short_closures`).
    #[test]
    fn leaves_plain_php_untouched() {
        let src = b"<?php\n$a = 1 + 2;\necho $a;\n";
        assert_eq!(transpile(src), src);
    }

    // Non-UTF-8 source (a Latin-1 identifier and comment) transpiles byte-exactly.
    #[test]
    fn handles_non_utf8_source() {
        // `caf\xe9` is a valid PHP identifier but invalid UTF-8.
        let src = b"<?php\n// caf\xe9\n$f = fn () => { return $caf\xe9; };\n";
        let out = transpile(src);
        // The captured non-UTF-8 variable is spliced into `use (...)` verbatim.
        assert!(out.windows(9).any(|w| w == b"use ($caf"), "got: {out:?}");
        assert!(out.contains(&0xe9), "non-UTF-8 byte preserved");
    }
}

//! pxp — a fast PHP superset transpiler.
//!
//! Architecture (pilot):
//!   source ──lex──▶ tokens+spans ──transform passes──▶ edits ──splice──▶ PHP
//!
//! The guiding principles:
//!   * **Span-splice, don't pretty-print.** Untouched code is copied verbatim, so
//!     formatting/comments are preserved and only changed regions cost anything.
//!   * **Line-count-preserving desugarings.** No source maps needed — line N in
//!     the source is line N in the output.

pub mod ast;
pub mod astcount;
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

/// Transpile a single source string of pxp into plain PHP.
pub fn transpile(src: &str) -> String {
    let edits = transform::run_all(src);
    emit::apply(src, edits).0
}

/// Transpile and also produce a [`SourceMap`] relating the generated PHP back to
/// the source, for translating exception/stack-trace lines.
pub fn transpile_with_map(
    src: &str,
    source_path: impl Into<String>,
    generated_path: impl Into<String>,
) -> (String, SourceMap) {
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
        let src = "<?php\n$a = 1 + 2;\necho $a;\n";
        assert_eq!(transpile(src), src);
    }
}

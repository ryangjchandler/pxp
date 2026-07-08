//! Multi-line short closures.
//!
//! Turns the (PHP-invalid) block-bodied arrow function
//!
//! ```php
//! $f = fn ($x) => {
//!     $y = $x * $factor;
//!     return $y;
//! };
//! ```
//!
//! into a real closure that preserves `fn`'s auto-capture-by-value semantics:
//!
//! ```php
//! $f = function ($x) use ($factor) {
//!     $y = $x * $factor;
//!     return $y;
//! };
//! ```
//!
//! The transform is line-count-preserving: only the header is rewritten
//! (`fn`→`function`, the `=>` is dropped, and the inferred `use (...)` is inserted
//! after the parameter list). Body lines are untouched, so stack traces and
//! Xdebug keep pointing at the right source line without any source map.
//!
//! The capture list comes from the [`crate::scope`] resolver — an exact
//! free-variable analysis over the parsed AST, so nested closures, destructuring,
//! `foreach`/`catch` bindings, and string interpolation are all handled correctly.

use crate::emit::Edit;
use crate::scope;

pub fn transform(src: &[u8]) -> Vec<Edit> {
    let (program, _errors) = crate::parser::parse(src);
    let mut edits = Vec::new();
    for closure in scope::resolve(&program) {
        let bf = closure.node;
        // `fn` -> `function`
        edits.push(Edit::replace(bf.fn_span.start, bf.fn_span.end, b"function".to_vec()));
        // Drop the `=>` (surrounding whitespace stays, so the line is preserved).
        edits.push(Edit::replace(bf.arrow_span.start, bf.arrow_span.end, Vec::new()));
        // Insert the synthesized `use (...)` right after the parameter list — which
        // is the correct spot even when a return type follows. Built as raw bytes
        // because a captured name may itself be non-UTF-8.
        if !closure.captures.is_empty() {
            let mut repl = b" use (".to_vec();
            for (i, name) in closure.captures.iter().enumerate() {
                if i > 0 {
                    repl.extend_from_slice(b", ");
                }
                repl.push(b'$');
                repl.extend_from_slice(name.as_bytes());
            }
            repl.push(b')');
            edits.push(Edit::insert(bf.params_end, repl));
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    // Feature-level tests exercise the full pipeline (parse + resolve + emit) via
    // `transpile_str`, so they assert the actual PHP a user would get.
    use crate::transpile_str;

    fn t(src: &str) -> String {
        transpile_str(src)
    }

    #[test]
    fn leaves_normal_arrow_fn_untouched() {
        let src = "<?php\n$f = fn ($x) => $x * 2;\n";
        assert_eq!(t(src), src);
    }

    #[test]
    fn block_body_with_no_captures() {
        let src = "<?php\n$f = fn ($x) => { return $x * 2; };\n";
        let out = t(src);
        assert!(out.contains("function ($x)"), "got: {out}");
        assert!(!out.contains("use ("), "should have no captures: {out}");
        assert!(!out.contains("=>"), "arrow should be gone: {out}");
    }

    #[test]
    fn captures_free_variable_by_value() {
        let src = "<?php\n$f = fn ($x) => { return $x * $factor; };\n";
        let out = t(src);
        assert!(out.contains("use ($factor)"), "got: {out}");
    }

    #[test]
    fn does_not_capture_locally_assigned() {
        let src = "<?php\n$f = fn () => { $y = 5; return $y; };\n";
        let out = t(src);
        assert!(!out.contains("use ("), "y is local: {out}");
    }

    #[test]
    fn captures_read_before_local_assign() {
        let src = "<?php\n$f = fn () => { $sum = $sum + 1; return $sum; };\n";
        let out = t(src);
        // $sum is read on the RHS before it is bound, so it must be captured.
        assert!(out.contains("use ($sum)"), "got: {out}");
    }

    #[test]
    fn excludes_this_and_superglobals() {
        let src = "<?php\n$f = fn () => { return $this->x + $_GET['a']; };\n";
        let out = t(src);
        assert!(!out.contains("use ("), "this/superglobals excluded: {out}");
    }

    #[test]
    fn foreach_targets_are_local() {
        let src = "<?php\n$f = fn () => { foreach ($rows as $k => $v) { echo $k . $v; } };\n";
        let out = t(src);
        assert!(out.contains("function () use ($rows)"), "got: {out}");
    }

    #[test]
    fn preserves_line_count() {
        let src = "<?php\n$f = fn ($x) => {\n    $y = $x * $factor;\n    return $y;\n};\n";
        let out = t(src);
        assert_eq!(
            src.lines().count(),
            out.lines().count(),
            "line count must be preserved\n--- in ---\n{src}\n--- out ---\n{out}"
        );
    }

    #[test]
    fn brace_in_string_does_not_break_matching() {
        let src = "<?php\n$f = fn () => { $s = '}'; return $s . $tail; };\n";
        let out = t(src);
        assert!(out.contains("use ($tail)"), "got: {out}");
        assert!(!out.contains("use ($s"), "s is local: {out}");
    }

    #[test]
    fn captures_variable_interpolated_in_string() {
        let src = "<?php\n$f = fn () => { return \"value: $a\"; };\n";
        let out = t(src);
        assert!(out.contains("use ($a)"), "got: {out}");
    }

    #[test]
    fn captures_variable_in_complex_interpolation() {
        let src = "<?php\n$f = fn () => { return \"x {$obj->prop} y\"; };\n";
        let out = t(src);
        assert!(out.contains("use ($obj)"), "got: {out}");
    }

    // ---------------------------------------------------------------------
    // Retired limitations — fixed by the Phase D scope resolver.
    // ---------------------------------------------------------------------

    #[test]
    fn does_not_capture_inner_closure_params() {
        let src =
            "<?php\n$outer = fn () => { $ids = fn ($id) => { return $id; }; return $base; };\n";
        let out = t(src);
        // The outer closure captures only $base — $id belongs to the inner
        // closure's scope and must not leak into the outer `use (...)`.
        assert!(out.contains("function () use ($base)"), "got: {out}");
        assert!(!out.contains("$id,") && !out.contains("($id)  use"), "got: {out}");
    }

    #[test]
    fn destructuring_targets_are_local() {
        let src = "<?php\n$f = fn () => { [$a, $b] = $arr; return $a + $b; };\n";
        let out = t(src);
        // $a and $b are bound by the destructuring assignment; only $arr is free.
        assert!(out.contains("function () use ($arr)"), "got: {out}");
    }

    #[test]
    fn transitively_captures_through_nested_arrow_fn() {
        // The inner arrow fn reads $base from the outer scope, so the outer block
        // closure must capture $base to pass it through.
        let src = "<?php\n$outer = fn () => { $g = fn () => $base; return $g(); };\n";
        let out = t(src);
        assert!(out.contains("function () use ($base)"), "got: {out}");
    }
}

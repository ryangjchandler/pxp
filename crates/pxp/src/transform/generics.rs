//! Erased generics.
//!
//! Adds a lightweight generic syntax on top of PHP and erases it to plain PHP
//! plus PHPStan/Psalm-compatible docblocks:
//!
//! ```php
//! class Box<T> {
//!     private T $value;
//!     public function __construct(T $value) { $this->value = $value; }
//!     public function get(): T { return $this->value; }
//! }
//! $b = new Box::<User>();
//! ```
//!
//! becomes:
//!
//! ```php
//! /** @template T */ class Box {
//!     /** @var T */ private $value;
//!     /** @param T $value */ public function __construct($value) { $this->value = $value; }
//!     /** @return T */ public function get() { return $this->value; }
//! }
//! $b = new Box();
//! ```
//!
//! Two erasure rules:
//!
//! * **Generic application** (`Box<User>`, `array<int, User>`) — keep the base
//!   type natively, delete the `<...>`, and record the full text in the docblock.
//!   Runs on every type position, which is what makes `f(Box<User> $x)` work
//!   outside any generic class.
//! * **Type parameter** (`T`, scoped to a class declaring `<T>`) — strip the
//!   native type entirely and record the parameter name in the docblock.
//!
//! Generated tags are merged into a declaration's existing doc comment when it has
//! one; otherwise a fresh block is emitted. A single tag stays inline
//! (`/** ... */`), preserving line numbers; two or more require a multi-line block,
//! whose line shift the source map translates back for stack traces.
//!
//! Only single-level applications are handled: a nested `Foo<Bar<T>>` (closing with
//! a `>>` token) is not supported. Closure and arrow-fn parameter types are erased
//! to stay valid PHP but are not annotated.

use crate::ast::*;
use crate::bytestr::ByteString;
use crate::emit::Edit;
use crate::span::Span;

pub fn transform(src: &[u8]) -> Vec<Edit> {
    let (program, _errors) = crate::parser::parse(src);
    let mut cx = Cx { src, edits: Vec::new() };
    let no_params: Vec<ByteString> = Vec::new();
    cx.stmts(&program.stmts, &no_params);
    cx.edits
}

struct Cx<'a> {
    src: &'a [u8],
    edits: Vec<Edit>,
}

/// How to erase an affected type from a native type position.
enum Erase {
    /// Keep the base name; delete only the `<...>` argument clause.
    ArgsOnly(Span),
    /// Delete the whole native type (bare parameter, `?T`, unions with a param).
    Whole,
}

impl<'a> Cx<'a> {
    fn slice(&self, sp: Span) -> &[u8] {
        &self.src[sp.start..sp.end]
    }

    // --- statements -------------------------------------------------------

    fn stmts(&mut self, list: &[Stmt], params: &[ByteString]) {
        for s in list {
            self.stmt(s, params);
        }
    }

    fn stmt(&mut self, s: &Stmt, params: &[ByteString]) {
        match &s.kind {
            StmtKind::Function(f) => self.function(f, params, f.span.start, f.doc),
            StmtKind::ClassLike(cl) => self.classlike(cl, s.span),
            StmtKind::Expr(e) => self.expr(e, params),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.expr(e, params);
                }
            }
            StmtKind::Echo(es) | StmtKind::Unset(es) => self.exprs(es, params),
            StmtKind::Block(b) => self.stmts(b, params),
            StmtKind::If { cond, then, else_ifs, else_ } => {
                self.expr(cond, params);
                self.stmts(then, params);
                for (cc, bb) in else_ifs {
                    self.expr(cc, params);
                    self.stmts(bb, params);
                }
                if let Some(b) = else_ {
                    self.stmts(b, params);
                }
            }
            StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                self.expr(cond, params);
                self.stmts(body, params);
            }
            StmtKind::For { init, cond, update, body } => {
                self.exprs(init, params);
                self.exprs(cond, params);
                self.exprs(update, params);
                self.stmts(body, params);
            }
            StmtKind::Foreach { subject, key, value, body, .. } => {
                self.expr(subject, params);
                if let Some(k) = key {
                    self.expr(k, params);
                }
                self.expr(value, params);
                self.stmts(body, params);
            }
            StmtKind::Switch { subject, cases } => {
                self.expr(subject, params);
                for case in cases {
                    if let Some(t) = &case.test {
                        self.expr(t, params);
                    }
                    self.stmts(&case.body, params);
                }
            }
            StmtKind::Try { body, catches, finally } => {
                self.stmts(body, params);
                for cat in catches {
                    self.stmts(&cat.body, params);
                }
                if let Some(f) = finally {
                    self.stmts(f, params);
                }
            }
            StmtKind::Break(e) | StmtKind::Continue(e) => {
                if let Some(e) = e {
                    self.expr(e, params);
                }
            }
            StmtKind::StaticVars(vars) => {
                for (_, def) in vars {
                    if let Some(d) = def {
                        self.expr(d, params);
                    }
                }
            }
            StmtKind::Const(consts) => {
                for (_, v) in consts {
                    self.expr(v, params);
                }
            }
            StmtKind::Declare { body, .. } | StmtKind::Namespace { body, .. } => {
                if let Some(b) = body {
                    self.stmts(b, params);
                }
            }
            StmtKind::Global(_)
            | StmtKind::Goto(_)
            | StmtKind::Label(_)
            | StmtKind::Use(_)
            | StmtKind::InlineHtml(_)
            | StmtKind::OpenTag
            | StmtKind::CloseTag
            | StmtKind::Nop
            | StmtKind::Unknown(_) => {}
        }
    }

    // --- declarations -----------------------------------------------------

    fn classlike(&mut self, cl: &ClassLike, span: Span) {
        // A named generic class contributes `@template` tags and drops its `<...>`.
        // Anonymous classes never declare type parameters (their `params` stays
        // empty), so only generic-application erasure runs on their members.
        if !cl.type_params.is_empty() {
            let tags: Vec<Vec<u8>> = cl
                .type_params
                .iter()
                .map(|p| {
                    let mut t = b"@template ".to_vec();
                    t.extend_from_slice(p.name.as_bytes());
                    t
                })
                .collect();
            self.emit_docblock(span.start, cl.doc, &tags);
            if let Some(tps) = cl.type_params_span {
                self.edits.push(Edit::replace(tps.start, tps.end, Vec::new()));
            }
        }

        let params: Vec<ByteString> = cl.type_params.iter().map(|p| p.name.clone()).collect();
        for m in &cl.members {
            self.member(m, &params);
        }
    }

    fn member(&mut self, m: &Member, params: &[ByteString]) {
        match &m.kind {
            MemberKind::Property { ty: Some(ty), .. } | MemberKind::Const { ty: Some(ty), .. } => {
                if let Some(er) = classify(ty, params) {
                    let mut tag = b"@var ".to_vec();
                    tag.extend_from_slice(self.slice(ty.span));
                    self.emit_docblock(m.span.start, m.doc, &[tag]);
                    self.erase(ty, er, false);
                }
            }
            MemberKind::Method(f) => self.function(f, params, m.span.start, m.doc),
            MemberKind::Property { ty: None, .. }
            | MemberKind::Const { ty: None, .. }
            | MemberKind::EnumCase { .. }
            | MemberKind::UseTrait { .. } => {}
        }
    }

    /// A named function or method: erase parameter/return types in place and emit a
    /// single combined docblock (`@param`/`@return`) at `doc_at`. Closures don't
    /// come through here — they're erased natively without annotation.
    fn function(&mut self, f: &FunctionDecl, params: &[ByteString], doc_at: usize, doc: Option<Span>) {
        let mut tags: Vec<Vec<u8>> = Vec::new();
        for p in &f.params {
            if let Some(ty) = &p.ty {
                if let Some(er) = classify(ty, params) {
                    let mut tag = b"@param ".to_vec();
                    tag.extend_from_slice(self.slice(ty.span));
                    tag.extend_from_slice(b" $");
                    tag.extend_from_slice(p.name.as_bytes());
                    tags.push(tag);
                    self.erase(ty, er, false);
                }
            }
        }
        if let Some(rty) = &f.return_type {
            if let Some(er) = classify(rty, params) {
                let mut tag = b"@return ".to_vec();
                tag.extend_from_slice(self.slice(rty.span));
                tags.push(tag);
                self.erase(rty, er, true);
            }
        }
        self.emit_docblock(doc_at, doc, &tags);
        if let Some(body) = &f.body {
            self.stmts(body, params);
        }
    }

    /// Attach a docblock carrying `tags` to the declaration at `at`.
    ///
    /// If the declaration already has a `/** ... */` doc comment, the tags are
    /// merged *into* it — a separate block would shadow the original, since PHP
    /// (and PHPStan/Psalm) only read the doc comment adjacent to the declaration.
    /// Otherwise a fresh block is emitted: a single tag stays inline
    /// (`/** @return T */ `, line-count-preserving); two or more become a
    /// multi-line block (phpDoc parsers read only the first tag on a line). Any
    /// added lines are translated back for stack traces by the source map.
    fn emit_docblock(&mut self, at: usize, doc: Option<Span>, tags: &[Vec<u8>]) {
        if tags.is_empty() {
            return;
        }
        if let Some(d) = doc {
            self.merge_into_doc(d.start, d.end, tags);
            return;
        }
        match tags {
            [] => {}
            [only] => {
                let mut db = b"/** ".to_vec();
                db.extend_from_slice(only);
                db.extend_from_slice(b" */ ");
                self.edits.push(Edit::insert(at, db));
            }
            many => {
                let indent = self.line_indent(at).to_vec();
                let mut db = b"/**\n".to_vec();
                for t in many {
                    db.extend_from_slice(&indent);
                    db.extend_from_slice(b" * ");
                    db.extend_from_slice(t);
                    db.push(b'\n');
                }
                db.extend_from_slice(&indent);
                db.extend_from_slice(b" */\n");
                db.extend_from_slice(&indent);
                self.edits.push(Edit::insert(at, db));
            }
        }
    }

    /// Splice `tags` into the existing doc comment spanning `[ds, de)`, preserving
    /// its content. A multi-line block gets the tags as new ` * @tag` lines before
    /// its closing `*/`; a single-line block is expanded to multi-line with its
    /// text kept as the leading description line.
    fn merge_into_doc(&mut self, ds: usize, de: usize, tags: &[Vec<u8>]) {
        let indent = self.line_indent(ds).to_vec();
        let is_multiline = self.src[ds..de].contains(&b'\n');
        if is_multiline {
            // Start of the line the closing `*/` sits on.
            let mut cls = de - 2;
            while cls > ds && self.src[cls - 1] != b'\n' {
                cls -= 1;
            }
            let mut ins = Vec::new();
            for t in tags {
                ins.extend_from_slice(&indent);
                ins.extend_from_slice(b" * ");
                ins.extend_from_slice(t);
                ins.push(b'\n');
            }
            self.edits.push(Edit::insert(cls, ins));
        } else {
            let inner = trim_ascii(&self.src[ds + 3..de - 2]);
            let mut rep = b"/**\n".to_vec();
            if !inner.is_empty() {
                rep.extend_from_slice(&indent);
                rep.extend_from_slice(b" * ");
                rep.extend_from_slice(inner);
                rep.push(b'\n');
            }
            for t in tags {
                rep.extend_from_slice(&indent);
                rep.extend_from_slice(b" * ");
                rep.extend_from_slice(t);
                rep.push(b'\n');
            }
            rep.extend_from_slice(&indent);
            rep.extend_from_slice(b" */");
            self.edits.push(Edit::replace(ds, de, rep));
        }
    }

    /// The run of spaces/tabs immediately preceding `offset` — the indentation of
    /// the declaration's line, replicated on each line of a multi-line docblock.
    fn line_indent(&self, offset: usize) -> &[u8] {
        let mut i = offset;
        while i > 0 && matches!(self.src[i - 1], b' ' | b'\t') {
            i -= 1;
        }
        &self.src[i..offset]
    }

    /// Erase generic parameter/return types on a closure or arrow fn, keeping the
    /// result valid PHP. Anonymous functions get no docblock (nowhere clean to put
    /// one), so a bare `T` parameter loses its annotation.
    fn erase_anon_params(&mut self, ps: &[Param], ret: Option<&Type>, params: &[ByteString]) {
        for p in ps {
            if let Some(ty) = &p.ty {
                if let Some(er) = classify(ty, params) {
                    self.erase(ty, er, false);
                }
            }
        }
        if let Some(rty) = ret {
            if let Some(er) = classify(rty, params) {
                self.erase(rty, er, true);
            }
        }
    }

    fn erase(&mut self, ty: &Type, er: Erase, is_return: bool) {
        match er {
            Erase::ArgsOnly(sp) => self.edits.push(Edit::replace(sp.start, sp.end, Vec::new())),
            Erase::Whole => {
                // A stripped return type must take its `:` with it, or `: {` is a
                // syntax error. For parameter/property types, also swallow the single
                // trailing space so `T $x` becomes `$x`, not ` $x` — kept to spaces
                // and tabs so line numbers never move.
                let start = if is_return {
                    self.colon_before(ty.span.start)
                } else {
                    ty.span.start
                };
                let mut end = ty.span.end;
                if !is_return {
                    while end < self.src.len() && matches!(self.src[end], b' ' | b'\t') {
                        end += 1;
                    }
                }
                self.edits.push(Edit::replace(start, end, Vec::new()));
            }
        }
    }

    /// Walk left from a return type's start over whitespace to its `:` so the whole
    /// `: T` can be removed as a unit.
    fn colon_before(&self, type_start: usize) -> usize {
        let mut i = type_start;
        while i > 0 && self.src[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        if i > 0 && self.src[i - 1] == b':' {
            i - 1
        } else {
            type_start
        }
    }

    // --- expressions ------------------------------------------------------

    fn exprs(&mut self, list: &[Expr], params: &[ByteString]) {
        for e in list {
            self.expr(e, params);
        }
    }

    fn expr(&mut self, e: &Expr, params: &[ByteString]) {
        match &e.kind {
            ExprKind::New { class, args, type_args } => {
                // Drop the turbofish `::<...>` — leaving it would be a PHP syntax error.
                if let Some(ta) = type_args {
                    self.edits.push(Edit::replace(ta.span.start, ta.span.end, Vec::new()));
                }
                self.expr(class, params);
                for a in args {
                    self.expr(&a.value, params);
                }
            }
            ExprKind::NewAnon { args, class } => {
                self.classlike(class, class.span);
                for a in args {
                    self.expr(&a.value, params);
                }
            }
            ExprKind::Closure(cl) => {
                self.erase_anon_params(&cl.params, None, params);
                self.stmts(&cl.body, params);
            }
            ExprKind::ArrowFn(f) => {
                self.erase_anon_params(&f.params, None, params);
                self.expr(&f.body, params);
            }
            ExprKind::BlockArrowFn(f) => {
                self.erase_anon_params(&f.params, None, params);
                self.stmts(&f.body, params);
            }
            ExprKind::VariableVariable(inner) => self.expr(inner, params),
            ExprKind::Interpolated(parts) => {
                for p in parts {
                    if let StringPart::Expr(e) = p {
                        self.expr(e, params);
                    }
                }
            }
            ExprKind::Array(items) | ExprKind::List(items) => {
                for it in items {
                    if let Some(k) = &it.key {
                        self.expr(k, params);
                    }
                    if let Some(v) = &it.value {
                        self.expr(v, params);
                    }
                }
            }
            ExprKind::Unary { operand, .. } | ExprKind::PostfixIncDec { operand, .. } => {
                self.expr(operand, params)
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, params);
                self.expr(rhs, params);
            }
            ExprKind::Assign { target, value, .. } => {
                self.expr(target, params);
                self.expr(value, params);
            }
            ExprKind::Ternary { cond, then, else_ } => {
                self.expr(cond, params);
                if let Some(t) = then {
                    self.expr(t, params);
                }
                self.expr(else_, params);
            }
            ExprKind::Cast { operand, .. } => self.expr(operand, params),
            ExprKind::Instanceof { expr, class } => {
                self.expr(expr, params);
                self.expr(class, params);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee, params);
                for a in args {
                    self.expr(&a.value, params);
                }
            }
            ExprKind::Member { object, name, .. } => {
                self.expr(object, params);
                if let MemberName::Expr(e2) = name {
                    self.expr(e2, params);
                }
            }
            ExprKind::StaticMember { class, name } => {
                self.expr(class, params);
                if let MemberName::Expr(e2) = name {
                    self.expr(e2, params);
                }
            }
            ExprKind::Index { base, index } => {
                self.expr(base, params);
                if let Some(i) = index {
                    self.expr(i, params);
                }
            }
            ExprKind::Match(m) => {
                self.expr(&m.subject, params);
                for arm in &m.arms {
                    if let Some(conds) = &arm.conditions {
                        self.exprs(conds, params);
                    }
                    self.expr(&arm.body, params);
                }
            }
            ExprKind::Isset(xs) => self.exprs(xs, params),
            ExprKind::Empty(e2)
            | ExprKind::Clone(e2)
            | ExprKind::Print(e2)
            | ExprKind::Throw(e2)
            | ExprKind::FirstClassCallable(e2)
            | ExprKind::YieldFrom(e2) => self.expr(e2, params),
            ExprKind::Include { path, .. } => self.expr(path, params),
            ExprKind::Yield { key, value } => {
                if let Some(k) = key {
                    self.expr(k, params);
                }
                if let Some(v) = value {
                    self.expr(v, params);
                }
            }
            ExprKind::Int
            | ExprKind::Float
            | ExprKind::String
            | ExprKind::Variable(_)
            | ExprKind::Name(_)
            | ExprKind::Error => {}
        }
    }
}

/// Trim leading/trailing ASCII whitespace from a byte slice.
fn trim_ascii(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && s[a].is_ascii_whitespace() {
        a += 1;
    }
    while b > a && s[b - 1].is_ascii_whitespace() {
        b -= 1;
    }
    &s[a..b]
}

/// Decide how (if at all) a native type position must be erased.
fn classify(ty: &Type, params: &[ByteString]) -> Option<Erase> {
    match &ty.kind {
        // `Box<User>` / `array<int, User>` — keep the base, drop the arguments.
        TypeKind::Generic { args_span, .. } => Some(Erase::ArgsOnly(*args_span)),
        // Anything mentioning a type parameter (`T`, `?T`, `int|T`) goes wholesale
        // to the docblock — stripping the native hint is always sound.
        _ if contains_param(ty, params) => Some(Erase::Whole),
        _ => None,
    }
}

/// Whether a type mentions one of the in-scope type parameters anywhere.
fn contains_param(ty: &Type, params: &[ByteString]) -> bool {
    match &ty.kind {
        TypeKind::Named(name) => params.iter().any(|p| p == name),
        TypeKind::Nullable(inner) => contains_param(inner, params),
        TypeKind::Union(members) | TypeKind::Intersection(members) => {
            members.iter().any(|t| contains_param(t, params))
        }
        // A generic application is handled by `classify` before this is reached.
        TypeKind::Generic { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::transpile_str;

    fn t(src: &str) -> String {
        transpile_str(src)
    }

    #[test]
    fn simple_generic_class() {
        let src = "<?php\nclass Box<T> {\n    private T $value;\n    public function __construct(T $value) { $this->value = $value; }\n    public function get(): T { return $this->value; }\n}\n";
        let out = t(src);
        assert!(out.contains("/** @template T */ class Box {"), "got: {out}");
        assert!(out.contains("/** @var T */ private $value;"), "got: {out}");
        assert!(out.contains("/** @param T $value */"), "got: {out}");
        assert!(out.contains("__construct($value)"), "param type stripped: {out}");
        assert!(out.contains("/** @return T */"), "got: {out}");
        assert!(out.contains("public function get() {"), "return type stripped: {out}");
        assert!(!out.contains("<T>"), "type param decl not stripped: {out}");
    }

    #[test]
    fn preserves_line_count() {
        let src = "<?php\nclass Box<T> {\n    private T $value;\n    public function get(): T {\n        return $this->value;\n    }\n}\n";
        let out = t(src);
        assert_eq!(
            src.lines().count(),
            out.lines().count(),
            "line count must be preserved\n--- in ---\n{src}\n--- out ---\n{out}"
        );
    }

    #[test]
    fn generic_application_as_param_hint_in_free_function() {
        let src = "<?php\nfunction handle(Box<User> $box) { return $box; }\n";
        let out = t(src);
        assert!(out.contains("/** @param Box<User> $box */"), "got: {out}");
        assert!(out.contains("function handle(Box $box)"), "base kept, args stripped: {out}");
    }

    #[test]
    fn multiple_type_params_use_multiline_block() {
        // Two+ tags can't share a line (phpDoc parsers read only the first), so the
        // docblock becomes a normal multi-line block, indented to the declaration.
        let src = "<?php\nclass Pair<K, V> {\n    public function __construct(K $key, V $value) {}\n}\n";
        let out = t(src);
        assert!(
            out.contains("/**\n * @template K\n * @template V\n */\nclass Pair {"),
            "class template block:\n{out}"
        );
        assert!(
            out.contains(
                "    /**\n     * @param K $key\n     * @param V $value\n     */\n    public function __construct($key, $value)"
            ),
            "method param block:\n{out}"
        );
    }

    #[test]
    fn multi_tag_shift_is_recorded_in_source_map() {
        use crate::transpile_with_map;
        // `marker();` (source line 4) is pushed down by the multi-line @template and
        // @param blocks; the source map must translate the generated line back to 4.
        let src = "<?php\nclass Pair<K, V> {\n    public function __construct(K $key, V $value) {\n        marker();\n    }\n}\n";
        let (gen, map) = transpile_with_map(src, "a.pxp", "a.php");
        let gen = String::from_utf8(gen).unwrap();
        let gen_line = gen.lines().position(|l| l.contains("marker();")).unwrap() as u32 + 1;
        assert!(gen_line > 4, "expected a downward shift, got gen_line={gen_line}");
        assert!(!map.is_identity(), "multi-tag output shifts lines");
        assert_eq!(map.source_line(gen_line), 4, "generated line {gen_line} should map back to source line 4");
    }

    #[test]
    fn turbofish_construction() {
        let src = "<?php\n$b = new Box::<User>();\n";
        let out = t(src);
        assert!(out.contains("$b = new Box();"), "got: {out}");
        assert!(!out.contains("::<"), "turbofish not stripped: {out}");
    }

    #[test]
    fn array_generic_application() {
        let src = "<?php\nfunction ids(array<int, User> $users) {}\n";
        let out = t(src);
        assert!(out.contains("/** @param array<int, User> $users */"), "got: {out}");
        assert!(out.contains("function ids(array $users)"), "got: {out}");
    }

    #[test]
    fn nullable_type_parameter() {
        let src = "<?php\nclass Box<T> {\n    public function get(): ?T { return null; }\n}\n";
        let out = t(src);
        assert!(out.contains("/** @return ?T */"), "got: {out}");
        assert!(!out.contains("?T {"), "native nullable param not stripped: {out}");
    }

    #[test]
    fn non_generic_class_untouched() {
        let src = "<?php\nclass Plain {\n    private int $x;\n    public function get(): int { return $this->x; }\n}\n";
        let out = t(src);
        assert_eq!(out, src, "no generics => byte-identical");
    }

    #[test]
    fn merges_into_existing_multiline_class_docblock() {
        let src = "<?php\n/**\n * A box.\n * @author Ryan\n */\nclass Box<T> {\n    public function get(): T {}\n}\n";
        let out = t(src);
        // Existing content is kept and the template tag joins the same block.
        assert!(out.contains(" * A box.\n * @author Ryan\n * @template T\n */\nclass Box {"), "got:\n{out}");
        // No second, shadowing block was emitted.
        assert!(!out.contains("*/\n/**"), "should not orphan the original block:\n{out}");
    }

    #[test]
    fn merges_into_existing_singleline_property_docblock() {
        let src = "<?php\nclass Box<T> {\n    /** The value. */\n    private T $value;\n}\n";
        let out = t(src);
        // Single-line block expands to multi-line, keeping its text as a description.
        assert!(out.contains("    /**\n     * The value.\n     * @var T\n     */\n    private $value;"), "got:\n{out}");
    }

    #[test]
    fn merges_into_existing_method_docblock_keeping_other_tags() {
        let src = "<?php\nclass Box<T> {\n    /**\n     * Wrap it.\n     * @throws \\RuntimeException\n     */\n    public function __construct(T $value) {}\n}\n";
        let out = t(src);
        assert!(out.contains(" * @throws \\RuntimeException\n     * @param T $value\n     */"), "got:\n{out}");
    }

    #[test]
    fn merges_doc_placed_before_class_attribute() {
        // The doc comment sits before the attribute; parser-captured docs find it
        // (the old byte-scan, anchored at the class keyword, could not).
        let src = "<?php\n/**\n * A box.\n */\n#[Attr]\nclass Box<T> {\n    public function get(): T {}\n}\n";
        let out = t(src);
        assert!(out.contains(" * A box.\n * @template T\n */\n#[Attr]\nclass Box {"), "got:\n{out}");
        assert!(!out.contains("*/\n/**"), "should not orphan the original block:\n{out}");
    }

    #[test]
    fn merges_doc_whose_body_contains_a_block_comment_open() {
        // A `/*` in the doc text (with no closing `*/` before the real end) must not
        // confuse detection — the lexer tokenizes the whole `/** ... */` as one doc
        // comment, where the old byte-scan would have mistaken the inner `/*`.
        let src = "<?php\n/**\n * Example: a /* b\n */\nclass Box<T> {\n    public function get(): T {}\n}\n";
        let out = t(src);
        assert!(out.contains("@template T\n */\nclass Box {"), "got:\n{out}");
    }

    #[test]
    fn ignores_plain_block_comment() {
        // A non-doc `/* */` comment must not be merged into; a fresh block is emitted.
        let src = "<?php\nclass Box<T> {\n    /* not a docblock */\n    private T $value;\n}\n";
        let out = t(src);
        assert!(out.contains("/* not a docblock */"), "plain comment preserved: {out}");
        assert!(out.contains("/** @var T */ private $value;"), "fresh block emitted: {out}");
    }

    #[test]
    fn promoted_constructor_property() {
        let src = "<?php\nclass Box<T> {\n    public function __construct(private T $value) {}\n}\n";
        let out = t(src);
        assert!(out.contains("/** @param T $value */"), "got: {out}");
        assert!(out.contains("__construct(private $value)"), "got: {out}");
    }
}

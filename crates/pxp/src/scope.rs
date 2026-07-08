//! Lexical scope resolution: exact free-variable analysis over the AST.
//!
//! For each block-bodied short closure (`fn (...) => { ... }`) this computes the
//! variables it captures from the enclosing scope — the `use (...)` list needed
//! to lower it to a real closure. Unlike the earlier token-level heuristic, this
//! is scope-correct:
//!
//! * **Nested closures don't leak.** A nested closure contributes only its
//!   *external references* (what it itself captures) to the outer scope, never
//!   its own parameters/locals. This also gives correct *transitive* capture.
//! * **Bindings are bindings.** Assignment targets, `list()`/`[...]`
//!   destructuring, `foreach` targets, `catch` variables, `static`/`global`
//!   declarations bind — they are not reads.
//! * **Read-before-bind.** A variable is captured only if it is *read* before it
//!   is first bound in the scope (so `$sum = $sum + 1` captures `$sum`, but
//!   `$y = 5; return $y;` does not).
//! * `$this` and superglobals are never captured.

use std::collections::HashSet;

use crate::ast::*;
use crate::bytestr::ByteString;

/// A block short closure paired with its resolved capture list (bare names,
/// without the leading `$`, in first-read order).
pub struct BlockClosure<'a> {
    pub node: &'a BlockArrowFn,
    pub captures: Vec<ByteString>,
}

/// Find every block-bodied short closure in the program and resolve its captures.
pub fn resolve(program: &Program) -> Vec<BlockClosure<'_>> {
    let mut collected = Vec::new();
    let mut top = Scope::default();
    for stmt in &program.stmts {
        top.visit_stmt(stmt, &mut collected);
    }
    collected
}

// Names are compared as raw bytes (PHP identifiers aren't guaranteed UTF-8).
#[derive(Default)]
struct Scope {
    bound: HashSet<Vec<u8>>,
    captured: Vec<ByteString>,
    seen: HashSet<Vec<u8>>,
}

impl Scope {
    fn from_params(params: &[Param]) -> Self {
        let mut s = Scope::default();
        for p in params {
            s.bind(p.name.as_bytes());
        }
        s
    }

    fn read(&mut self, name: &[u8]) {
        if name.is_empty() || name == b"this" || is_superglobal(name) {
            return;
        }
        if self.bound.contains(name) || self.seen.contains(name) {
            return;
        }
        self.captured.push(ByteString::from(name));
        self.seen.insert(name.to_vec());
    }

    fn bind(&mut self, name: &[u8]) {
        if !name.is_empty() {
            self.bound.insert(name.to_vec());
        }
    }

    // --- statements -------------------------------------------------------

    fn visit_stmt<'a>(&mut self, stmt: &'a Stmt, out: &mut Vec<BlockClosure<'a>>) {
        match &stmt.kind {
            StmtKind::Expr(e) => self.visit_expr(e, out),
            StmtKind::Echo(es) => self.visit_all(es, out),
            StmtKind::Return(e) => {
                if let Some(e) = e {
                    self.visit_expr(e, out);
                }
            }
            StmtKind::Block(b) => self.visit_stmts(b, out),
            StmtKind::If { cond, then, else_ifs, else_ } => {
                self.visit_expr(cond, out);
                self.visit_stmts(then, out);
                for (c, b) in else_ifs {
                    self.visit_expr(c, out);
                    self.visit_stmts(b, out);
                }
                if let Some(b) = else_ {
                    self.visit_stmts(b, out);
                }
            }
            StmtKind::While { cond, body } => {
                self.visit_expr(cond, out);
                self.visit_stmts(body, out);
            }
            StmtKind::DoWhile { body, cond } => {
                self.visit_stmts(body, out);
                self.visit_expr(cond, out);
            }
            StmtKind::For { init, cond, update, body } => {
                self.visit_all(init, out);
                self.visit_all(cond, out);
                self.visit_all(update, out);
                self.visit_stmts(body, out);
            }
            StmtKind::Foreach { subject, key, by_ref: _, value, body } => {
                self.visit_expr(subject, out);
                if let Some(k) = key {
                    self.bind_target(k, out);
                }
                self.bind_target(value, out);
                self.visit_stmts(body, out);
            }
            StmtKind::Switch { subject, cases } => {
                self.visit_expr(subject, out);
                for case in cases {
                    if let Some(t) = &case.test {
                        self.visit_expr(t, out);
                    }
                    self.visit_stmts(&case.body, out);
                }
            }
            StmtKind::Try { body, catches, finally } => {
                self.visit_stmts(body, out);
                for c in catches {
                    if let Some(v) = &c.var {
                        self.bind(v.as_bytes());
                    }
                    self.visit_stmts(&c.body, out);
                }
                if let Some(f) = finally {
                    self.visit_stmts(f, out);
                }
            }
            StmtKind::Break(e) | StmtKind::Continue(e) => {
                if let Some(e) = e {
                    self.visit_expr(e, out);
                }
            }
            StmtKind::Global(names) => {
                for n in names {
                    self.bind(n.as_bytes());
                }
            }
            StmtKind::StaticVars(vars) => {
                for (n, def) in vars {
                    if let Some(d) = def {
                        self.visit_expr(d, out);
                    }
                    self.bind(n.as_bytes());
                }
            }
            StmtKind::Unset(items) => self.visit_all(items, out),
            StmtKind::Const(consts) => {
                for (_, v) in consts {
                    self.visit_expr(v, out);
                }
            }
            StmtKind::Declare { body, .. } | StmtKind::Namespace { body, .. } => {
                if let Some(b) = body {
                    self.visit_stmts(b, out);
                }
            }
            // Nested declarations are separate scopes: traverse only to collect
            // block closures inside them (their captures don't affect us).
            StmtKind::Function(f) => {
                if let Some(body) = &f.body {
                    let _ = analyze_block(&f.params, body, out);
                }
            }
            StmtKind::ClassLike(c) => visit_class(c, out),
            StmtKind::Goto(_)
            | StmtKind::Label(_)
            | StmtKind::Use(_)
            | StmtKind::InlineHtml(_)
            | StmtKind::OpenTag
            | StmtKind::CloseTag
            | StmtKind::Nop
            | StmtKind::Unknown(_) => {}
        }
    }

    fn visit_stmts<'a>(&mut self, stmts: &'a [Stmt], out: &mut Vec<BlockClosure<'a>>) {
        for s in stmts {
            self.visit_stmt(s, out);
        }
    }

    fn visit_all<'a>(&mut self, exprs: &'a [Expr], out: &mut Vec<BlockClosure<'a>>) {
        for e in exprs {
            self.visit_expr(e, out);
        }
    }

    // --- expressions (read context) --------------------------------------

    fn visit_expr<'a>(&mut self, expr: &'a Expr, out: &mut Vec<BlockClosure<'a>>) {
        match &expr.kind {
            ExprKind::Variable(n) => self.read(n.as_bytes()),
            ExprKind::VariableVariable(inner) => self.visit_expr(inner, out),
            ExprKind::Int
            | ExprKind::Float
            | ExprKind::String
            | ExprKind::Name(_)
            | ExprKind::Error => {}
            ExprKind::Interpolated(parts) => {
                for p in parts {
                    if let StringPart::Expr(e) = p {
                        self.visit_expr(e, out);
                    }
                }
            }
            ExprKind::Array(items) | ExprKind::List(items) => {
                for it in items {
                    if let Some(k) = &it.key {
                        self.visit_expr(k, out);
                    }
                    if let Some(v) = &it.value {
                        self.visit_expr(v, out);
                    }
                }
            }
            ExprKind::Unary { operand, .. } | ExprKind::PostfixIncDec { operand, .. } => {
                self.visit_expr(operand, out)
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.visit_expr(lhs, out);
                self.visit_expr(rhs, out);
            }
            ExprKind::Assign { op, target, value } => {
                self.visit_expr(value, out);
                if op.is_some() {
                    // Compound assignment reads the target as well as binding it.
                    self.visit_expr(target, out);
                }
                self.bind_target(target, out);
            }
            ExprKind::Ternary { cond, then, else_ } => {
                self.visit_expr(cond, out);
                if let Some(t) = then {
                    self.visit_expr(t, out);
                }
                self.visit_expr(else_, out);
            }
            ExprKind::Cast { operand, .. } => self.visit_expr(operand, out),
            ExprKind::Instanceof { expr, class } => {
                self.visit_expr(expr, out);
                self.visit_expr(class, out);
            }
            ExprKind::Call { callee, args } => {
                self.visit_expr(callee, out);
                self.visit_args(args, out);
            }
            ExprKind::Member { object, name, .. } => {
                self.visit_expr(object, out);
                if let MemberName::Expr(e) = name {
                    self.visit_expr(e, out);
                }
            }
            ExprKind::StaticMember { class, name } => {
                self.visit_expr(class, out);
                if let MemberName::Expr(e) = name {
                    self.visit_expr(e, out);
                }
            }
            ExprKind::Index { base, index } => {
                self.visit_expr(base, out);
                if let Some(i) = index {
                    self.visit_expr(i, out);
                }
            }
            ExprKind::New { class, args } => {
                self.visit_expr(class, out);
                self.visit_args(args, out);
            }
            ExprKind::NewAnon { args, class } => {
                self.visit_args(args, out);
                visit_class(class, out);
            }
            ExprKind::Closure(c) => {
                // A real closure captures only via its `use` list — those names are
                // reads here. Its body is a separate scope; traverse it just to
                // collect nested block closures.
                for (name, _) in &c.uses {
                    self.read(name.as_bytes());
                }
                let mut inner = Scope::from_params(&c.params);
                for (name, _) in &c.uses {
                    inner.bind(name.as_bytes());
                }
                inner.visit_stmts(&c.body, out);
            }
            ExprKind::ArrowFn(f) => {
                // Auto-captures: its body's free variables (with its params bound)
                // are reads in this scope.
                let caps = analyze_expr(&f.params, &f.body, out);
                for n in &caps {
                    self.read(n.as_bytes());
                }
            }
            ExprKind::BlockArrowFn(bf) => {
                let caps = analyze_block(&bf.params, &bf.body, out);
                for n in &caps {
                    self.read(n.as_bytes());
                }
                out.push(BlockClosure { node: bf, captures: caps });
            }
            ExprKind::Match(m) => {
                self.visit_expr(&m.subject, out);
                for arm in &m.arms {
                    if let Some(conds) = &arm.conditions {
                        self.visit_all(conds, out);
                    }
                    self.visit_expr(&arm.body, out);
                }
            }
            ExprKind::Isset(xs) => self.visit_all(xs, out),
            ExprKind::Empty(e)
            | ExprKind::Clone(e)
            | ExprKind::Print(e)
            | ExprKind::Throw(e)
            | ExprKind::FirstClassCallable(e)
            | ExprKind::YieldFrom(e) => self.visit_expr(e, out),
            ExprKind::Include { path, .. } => self.visit_expr(path, out),
            ExprKind::Yield { key, value } => {
                if let Some(k) = key {
                    self.visit_expr(k, out);
                }
                if let Some(v) = value {
                    self.visit_expr(v, out);
                }
            }
        }
    }

    fn visit_args<'a>(&mut self, args: &'a [Arg], out: &mut Vec<BlockClosure<'a>>) {
        for a in args {
            self.visit_expr(&a.value, out);
        }
    }

    /// An assignment / loop target: bind plain variables and destructuring
    /// elements; treat index/member/dynamic targets as reads of their base.
    fn bind_target<'a>(&mut self, target: &'a Expr, out: &mut Vec<BlockClosure<'a>>) {
        match &target.kind {
            ExprKind::Variable(n) => self.bind(n.as_bytes()),
            ExprKind::Array(items) | ExprKind::List(items) => {
                for it in items {
                    if let Some(k) = &it.key {
                        self.visit_expr(k, out); // keys are reads
                    }
                    if let Some(v) = &it.value {
                        self.bind_target(v, out);
                    }
                }
            }
            ExprKind::Index { base, index } => {
                self.visit_expr(base, out);
                if let Some(i) = index {
                    self.visit_expr(i, out);
                }
            }
            ExprKind::Member { object, .. } => self.visit_expr(object, out),
            ExprKind::StaticMember { class, .. } => self.visit_expr(class, out),
            _ => self.visit_expr(target, out),
        }
    }
}

fn analyze_block<'a>(
    params: &[Param],
    body: &'a [Stmt],
    out: &mut Vec<BlockClosure<'a>>,
) -> Vec<ByteString> {
    let mut s = Scope::from_params(params);
    s.visit_stmts(body, out);
    s.captured
}

fn analyze_expr<'a>(
    params: &[Param],
    body: &'a Expr,
    out: &mut Vec<BlockClosure<'a>>,
) -> Vec<ByteString> {
    let mut s = Scope::from_params(params);
    s.visit_expr(body, out);
    s.captured
}

/// Traverse a class/enum/trait body purely to collect block closures nested in
/// method bodies, hooks, and member initializers (each a separate scope).
fn visit_class<'a>(class: &'a ClassLike, out: &mut Vec<BlockClosure<'a>>) {
    for m in &class.members {
        match &m.kind {
            MemberKind::Method(f) => {
                if let Some(body) = &f.body {
                    let _ = analyze_block(&f.params, body, out);
                }
            }
            MemberKind::Property { props, hooks, .. } => {
                for (_, def) in props {
                    if let Some(d) = def {
                        let _ = analyze_expr(&[], d, out);
                    }
                }
                for h in hooks {
                    match &h.body {
                        HookBody::Expr(e) => {
                            let _ = analyze_expr(&h.params, e, out);
                        }
                        HookBody::Block(b) => {
                            let _ = analyze_block(&h.params, b, out);
                        }
                        HookBody::None => {}
                    }
                }
            }
            MemberKind::Const { consts, .. } => {
                for (_, v) in consts {
                    let _ = analyze_expr(&[], v, out);
                }
            }
            MemberKind::EnumCase { value: Some(v), .. } => {
                let _ = analyze_expr(&[], v, out);
            }
            MemberKind::EnumCase { .. } | MemberKind::UseTrait { .. } => {}
        }
    }
}

fn is_superglobal(name: &[u8]) -> bool {
    matches!(
        name,
        b"GLOBALS"
            | b"_SERVER"
            | b"_GET"
            | b"_POST"
            | b"_FILES"
            | b"_COOKIE"
            | b"_SESSION"
            | b"_REQUEST"
            | b"_ENV"
    )
}

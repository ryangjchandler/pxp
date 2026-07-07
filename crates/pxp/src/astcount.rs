//! Structural node counts over the AST — a coarse but formatting-robust
//! fingerprint used to differential-test the parser against a reference
//! (nikic/PHP-Parser). Counts every declaration and closure exactly where a
//! reference AST walker would, so equal counts across a corpus is strong
//! evidence the two parsers agree on structure.
//!
//! Closures and arrow functions only ever appear in runtime expression positions
//! (never in constant contexts like param/const/property defaults), so the
//! traversal below — which mirrors the scope resolver's — sees all of them.

use crate::ast::*;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Counts {
    pub func: u32,
    pub class: u32,
    pub iface: u32,
    pub trait_: u32,
    pub enum_: u32,
    pub method: u32,
    pub prop: u32,
    pub const_: u32,
    pub case: u32,
    pub closure: u32,
    pub arrow: u32,
}

impl Counts {
    /// In the fixed order shared with the reference dumper.
    pub fn as_row(&self) -> [u32; 11] {
        [
            self.func, self.class, self.iface, self.trait_, self.enum_, self.method, self.prop,
            self.const_, self.case, self.closure, self.arrow,
        ]
    }

    pub const LABELS: [&'static str; 11] = [
        "func", "class", "iface", "trait", "enum", "method", "prop", "const", "case", "closure",
        "arrow",
    ];
}

pub fn count(program: &Program) -> Counts {
    let mut c = Counts::default();
    stmts(&program.stmts, &mut c);
    c
}

fn stmts(list: &[Stmt], c: &mut Counts) {
    for s in list {
        stmt(s, c);
    }
}

fn exprs(list: &[Expr], c: &mut Counts) {
    for e in list {
        expr(e, c);
    }
}

fn stmt(s: &Stmt, c: &mut Counts) {
    match &s.kind {
        StmtKind::Function(f) => {
            c.func += 1;
            if let Some(b) = &f.body {
                stmts(b, c);
            }
        }
        StmtKind::ClassLike(cl) => classlike(cl, c),
        StmtKind::Expr(e) => expr(e, c),
        StmtKind::Return(e) => {
            if let Some(e) = e {
                expr(e, c);
            }
        }
        StmtKind::Echo(es) | StmtKind::Unset(es) => exprs(es, c),
        StmtKind::Block(b) => stmts(b, c),
        StmtKind::If { cond, then, else_ifs, else_ } => {
            expr(cond, c);
            stmts(then, c);
            for (cc, bb) in else_ifs {
                expr(cc, c);
                stmts(bb, c);
            }
            if let Some(b) = else_ {
                stmts(b, c);
            }
        }
        StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
            expr(cond, c);
            stmts(body, c);
        }
        StmtKind::For { init, cond, update, body } => {
            exprs(init, c);
            exprs(cond, c);
            exprs(update, c);
            stmts(body, c);
        }
        StmtKind::Foreach { subject, key, value, body, .. } => {
            expr(subject, c);
            if let Some(k) = key {
                expr(k, c);
            }
            expr(value, c);
            stmts(body, c);
        }
        StmtKind::Switch { subject, cases } => {
            expr(subject, c);
            for case in cases {
                if let Some(t) = &case.test {
                    expr(t, c);
                }
                stmts(&case.body, c);
            }
        }
        StmtKind::Try { body, catches, finally } => {
            stmts(body, c);
            for cat in catches {
                stmts(&cat.body, c);
            }
            if let Some(f) = finally {
                stmts(f, c);
            }
        }
        StmtKind::Break(e) | StmtKind::Continue(e) => {
            if let Some(e) = e {
                expr(e, c);
            }
        }
        StmtKind::StaticVars(vars) => {
            for (_, def) in vars {
                if let Some(d) = def {
                    expr(d, c);
                }
            }
        }
        StmtKind::Const(consts) => {
            for (_, v) in consts {
                expr(v, c);
            }
        }
        StmtKind::Declare { body, .. } | StmtKind::Namespace { body, .. } => {
            if let Some(b) = body {
                stmts(b, c);
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

fn classlike(cl: &ClassLike, c: &mut Counts) {
    // Anonymous classes (name == None) aren't counted as a declaration, but their
    // members still are — matching how the reference walker treats them.
    if cl.name.is_some() {
        match cl.kind {
            ClassKind::Class => c.class += 1,
            ClassKind::Interface => c.iface += 1,
            ClassKind::Trait => c.trait_ += 1,
            ClassKind::Enum => c.enum_ += 1,
        }
    }
    for m in &cl.members {
        match &m.kind {
            MemberKind::Method(f) => {
                c.method += 1;
                for p in &f.params {
                    hooks(&p.hooks, c); // promoted-property hooks (8.4)
                }
                if let Some(b) = &f.body {
                    stmts(b, c);
                }
            }
            MemberKind::Property { props, hooks: hks, .. } => {
                c.prop += props.len() as u32;
                hooks(hks, c);
            }
            MemberKind::Const { consts, .. } => c.const_ += consts.len() as u32,
            MemberKind::EnumCase { .. } => c.case += 1,
            MemberKind::UseTrait { .. } => {}
        }
    }
}

fn hooks(list: &[PropertyHook], c: &mut Counts) {
    for h in list {
        match &h.body {
            HookBody::Expr(e) => expr(e, c),
            HookBody::Block(b) => stmts(b, c),
            HookBody::None => {}
        }
    }
}

fn expr(e: &Expr, c: &mut Counts) {
    match &e.kind {
        ExprKind::Closure(cl) => {
            c.closure += 1;
            stmts(&cl.body, c);
        }
        ExprKind::ArrowFn(f) => {
            c.arrow += 1;
            expr(&f.body, c);
        }
        ExprKind::BlockArrowFn(f) => {
            c.arrow += 1;
            stmts(&f.body, c);
        }
        ExprKind::NewAnon { args, class } => {
            classlike(class, c);
            for a in args {
                expr(&a.value, c);
            }
        }
        ExprKind::VariableVariable(inner) => expr(inner, c),
        ExprKind::Interpolated(parts) => {
            for p in parts {
                if let StringPart::Expr(e) = p {
                    expr(e, c);
                }
            }
        }
        ExprKind::Array(items) | ExprKind::List(items) => {
            for it in items {
                if let Some(k) = &it.key {
                    expr(k, c);
                }
                if let Some(v) = &it.value {
                    expr(v, c);
                }
            }
        }
        ExprKind::Unary { operand, .. } | ExprKind::PostfixIncDec { operand, .. } => expr(operand, c),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr(lhs, c);
            expr(rhs, c);
        }
        ExprKind::Assign { target, value, .. } => {
            expr(target, c);
            expr(value, c);
        }
        ExprKind::Ternary { cond, then, else_ } => {
            expr(cond, c);
            if let Some(t) = then {
                expr(t, c);
            }
            expr(else_, c);
        }
        ExprKind::Cast { operand, .. } => expr(operand, c),
        ExprKind::Instanceof { expr: e2, class } => {
            expr(e2, c);
            expr(class, c);
        }
        ExprKind::Call { callee, args } | ExprKind::New { class: callee, args } => {
            expr(callee, c);
            for a in args {
                expr(&a.value, c);
            }
        }
        ExprKind::Member { object, name, .. } => {
            expr(object, c);
            if let MemberName::Expr(e2) = name {
                expr(e2, c);
            }
        }
        ExprKind::StaticMember { class, name } => {
            expr(class, c);
            if let MemberName::Expr(e2) = name {
                expr(e2, c);
            }
        }
        ExprKind::Index { base, index } => {
            expr(base, c);
            if let Some(i) = index {
                expr(i, c);
            }
        }
        ExprKind::Match(m) => {
            expr(&m.subject, c);
            for arm in &m.arms {
                if let Some(conds) = &arm.conditions {
                    exprs(conds, c);
                }
                expr(&arm.body, c);
            }
        }
        ExprKind::Isset(xs) => exprs(xs, c),
        ExprKind::Empty(e2)
        | ExprKind::Clone(e2)
        | ExprKind::Print(e2)
        | ExprKind::Throw(e2)
        | ExprKind::FirstClassCallable(e2)
        | ExprKind::YieldFrom(e2) => expr(e2, c),
        ExprKind::Include { path, .. } => expr(path, c),
        ExprKind::Yield { key, value } => {
            if let Some(k) = key {
                expr(k, c);
            }
            if let Some(v) = value {
                expr(v, c);
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

#[cfg(test)]
mod tests {
    use super::count;
    use crate::parser::parse;

    #[test]
    fn counts_declarations_and_closures_incl_hooks() {
        let src = r#"<?php
        function top() { $f = fn () => 1; return $f; }
        class C {
            public const A = 1, B = 2;
            public int $x = 0;
            public int $y { get => fn () => 9; }
            public function m() { return function () { return 1; }; }
        }
        interface I {}
        trait T {}
        enum E { case X; case Y; }
        $g = fn ($a) => $a;
        "#;
        let (program, _errors) = parse(src);
        let c = count(&program);
        assert_eq!(c.func, 1);
        assert_eq!(c.class, 1);
        assert_eq!(c.iface, 1);
        assert_eq!(c.trait_, 1);
        assert_eq!(c.enum_, 1);
        assert_eq!(c.method, 1);
        assert_eq!(c.prop, 2, "$x and $y");
        assert_eq!(c.const_, 2, "A and B");
        assert_eq!(c.case, 2);
        assert_eq!(c.closure, 1, "function() in m()");
        assert_eq!(c.arrow, 3, "fn in top(), fn in the $y hook, and top-level $g");
    }
}

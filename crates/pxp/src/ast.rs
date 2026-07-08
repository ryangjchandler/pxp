//! A spanned, `Box`-based abstract syntax tree.
//!
//! Every node carries a [`Span`] into the original source. The tree is a *read*
//! model — the source buffer remains the lossless record, and emit works by
//! span-splicing, so the AST never needs to round-trip through a pretty-printer.
//!
//! Anything still not modelled parses into [`StmtKind::Unknown`] or
//! [`ExprKind::Error`] with a span covering the raw tokens, so the parser is
//! *total* (never panics, never loses bytes).

use crate::bytestr::ByteString;
use crate::span::Span;
use crate::token::Cast;

#[derive(Clone, Debug)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

// --- statements -----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `expr ;`
    Expr(Expr),
    Echo(Vec<Expr>),
    Return(Option<Expr>),
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        else_ifs: Vec<(Expr, Vec<Stmt>)>,
        else_: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    DoWhile {
        body: Vec<Stmt>,
        cond: Expr,
    },
    For {
        init: Vec<Expr>,
        cond: Vec<Expr>,
        update: Vec<Expr>,
        body: Vec<Stmt>,
    },
    Foreach {
        subject: Expr,
        key: Option<Expr>,
        by_ref: bool,
        value: Expr,
        body: Vec<Stmt>,
    },
    Switch {
        subject: Expr,
        cases: Vec<SwitchCase>,
    },
    Try {
        body: Vec<Stmt>,
        catches: Vec<Catch>,
        finally: Option<Vec<Stmt>>,
    },
    Break(Option<Expr>),
    Continue(Option<Expr>),
    Goto(ByteString),
    Label(ByteString),
    Global(Vec<ByteString>),
    /// `static $a = 1, $b;`
    StaticVars(Vec<(ByteString, Option<Expr>)>),
    Unset(Vec<Expr>),
    /// `const A = 1, B = 2;` (top-level / namespaced constant).
    Const(Vec<(ByteString, Expr)>),
    /// `declare(strict_types=1);` — directives kept as raw text, optional body.
    Declare {
        directives: Span,
        body: Option<Vec<Stmt>>,
    },
    Namespace {
        name: Option<ByteString>,
        body: Option<Vec<Stmt>>,
    },
    Use(UseDecl),
    Function(FunctionDecl),
    ClassLike(ClassLike),
    /// Raw inline HTML between `?>` and `<?php`.
    InlineHtml(Span),
    /// `<?php` / `?>` markers, kept so spans tile and emit stays lossless.
    OpenTag,
    CloseTag,
    /// A `;` on its own.
    Nop,
    /// A construct the parser doesn't model yet. Its span covers the raw tokens
    /// so downstream passes can still splice around it losslessly.
    Unknown(Span),
}

#[derive(Clone, Debug)]
pub struct SwitchCase {
    pub span: Span,
    /// `None` = the `default` case.
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Catch {
    pub span: Span,
    /// One or more caught types (union `catch (A | B $e)`).
    pub types: Vec<Type>,
    pub var: Option<ByteString>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct UseDecl {
    /// `use function`, `use const`, or plain class import.
    pub kind: UseKind,
    pub items: Vec<UseItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseKind {
    Normal,
    Function,
    Const,
}

#[derive(Clone, Debug)]
pub struct UseItem {
    pub span: Span,
    pub path: ByteString,
    pub alias: Option<ByteString>,
    /// Per-item kind for grouped `use A\{function b, const C}`.
    pub kind: Option<UseKind>,
}

// --- expressions ----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int,
    Float,
    /// A single-quoted or non-interpolated double-quoted string.
    String,
    /// A double-quoted / heredoc string with interpolation.
    Interpolated(Vec<StringPart>),
    /// `$name` (the name excludes the `$`).
    Variable(ByteString),
    /// `$$name` / `${expr}` — a variable whose name is computed.
    VariableVariable(Box<Expr>),
    /// A bareword: constant, function name, or class name — including `true`,
    /// `false`, `null`, `self`, `parent`, and qualified names like `A\B`.
    Name(ByteString),

    Array(Vec<ArrayItem>),

    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    /// Postfix `++`/`--`.
    PostfixIncDec {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Assign {
        op: Option<BinaryOp>, // None = plain `=`; Some = compound like `+=`
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `cond ? then : else`; `then` is `None` for the short ternary `?:`.
    Ternary {
        cond: Box<Expr>,
        then: Option<Box<Expr>>,
        else_: Box<Expr>,
    },
    Cast {
        cast: Cast,
        operand: Box<Expr>,
    },
    Instanceof {
        expr: Box<Expr>,
        class: Box<Expr>,
    },

    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    /// `object->name` / `object?->name` (property or method depending on a call).
    Member {
        object: Box<Expr>,
        nullsafe: bool,
        name: MemberName,
    },
    /// `class::member` — property (`::$x`), constant (`::X`), or method target.
    StaticMember {
        class: Box<Expr>,
        name: MemberName,
    },
    Index {
        base: Box<Expr>,
        index: Option<Box<Expr>>,
    },
    New {
        class: Box<Expr>,
        args: Vec<Arg>,
    },
    /// `new class (...) extends X implements Y { ... }`.
    NewAnon {
        args: Vec<Arg>,
        class: Box<ClassLike>,
    },

    Closure(Box<Closure>),
    ArrowFn(Box<ArrowFn>),
    /// The pxp extension: a block-bodied short closure `fn (...) => { ... }`.
    /// Lowered to a real `function (...) use (...) { ... }` by the transform.
    BlockArrowFn(Box<BlockArrowFn>),
    Match(Box<Match>),

    Isset(Vec<Expr>),
    Empty(Box<Expr>),
    List(Vec<ArrayItem>),

    Clone(Box<Expr>),
    Print(Box<Expr>),
    Throw(Box<Expr>),
    /// `include` / `include_once` / `require` / `require_once`.
    Include {
        kind: IncludeKind,
        path: Box<Expr>,
    },
    /// First-class callable syntax: `strlen(...)`, `$obj->m(...)`.
    FirstClassCallable(Box<Expr>),
    Yield {
        key: Option<Box<Expr>>,
        value: Option<Box<Expr>>,
    },
    YieldFrom(Box<Expr>),

    /// A recovered parse error. Its span covers the tokens skipped.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncludeKind {
    Include,
    IncludeOnce,
    Require,
    RequireOnce,
}

#[derive(Clone, Debug)]
pub enum StringPart {
    /// Literal text (an encapsed chunk).
    Literal(Span),
    /// An interpolated expression (`$a`, `{$a->b}`, `${name}`).
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct ArrayItem {
    pub span: Span,
    pub key: Option<Expr>,
    pub value: Option<Expr>, // None for a skipped element in destructuring
    pub by_ref: bool,
    pub spread: bool,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub span: Span,
    pub name: Option<ByteString>, // named argument `name: value`
    pub value: Expr,
    pub spread: bool,
}

/// The member selector after `->`/`::`.
#[derive(Clone, Debug)]
pub enum MemberName {
    /// `->prop`, `::CONST`, `->method(...)`.
    Identifier(ByteString),
    /// `->$prop`, `::$prop`.
    Variable(ByteString),
    /// `->{expr}`, `::{expr}`.
    Expr(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub span: Span,
    pub attrs: Vec<AttributeGroup>,
    /// Constructor promotion + asymmetric visibility modifiers, if any.
    pub modifiers: Vec<Modifier>,
    pub ty: Option<Type>,
    pub by_ref: bool,
    pub variadic: bool,
    pub name: ByteString,
    pub default: Option<Expr>,
    /// Property hooks on a promoted property (8.4).
    pub hooks: Vec<PropertyHook>,
}

#[derive(Clone, Debug)]
pub struct Closure {
    pub span: Span,
    pub is_static: bool,
    pub by_ref: bool,
    pub params: Vec<Param>,
    /// `use (...)` captures: (name, by_ref).
    pub uses: Vec<(ByteString, bool)>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct ArrowFn {
    pub span: Span,
    pub is_static: bool,
    pub by_ref: bool,
    pub params: Vec<Param>,
    pub body: Expr,
}

/// A block-bodied short closure (pxp syntax). Carries the token spans the
/// transform needs to lower it to a real closure while preserving line numbers.
#[derive(Clone, Debug)]
pub struct BlockArrowFn {
    pub span: Span,
    pub is_static: bool,
    pub by_ref: bool,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    /// Span of the `fn` keyword (rewritten to `function`).
    pub fn_span: Span,
    /// Span of the `=>` (deleted).
    pub arrow_span: Span,
    /// Byte offset just after the parameter list `)` — where `use (...)` is inserted.
    pub params_end: usize,
}

#[derive(Clone, Debug)]
pub struct Match {
    pub span: Span,
    pub subject: Expr,
    pub arms: Vec<MatchArm>,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub span: Span,
    /// `None` = the `default` arm.
    pub conditions: Option<Vec<Expr>>,
    pub body: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Not,     // !
    Neg,     // -
    Pos,     // +
    BitNot,  // ~
    Suppress, // @
    Reference, // &expr (reference-of, e.g. `$a = &$b`)
    PreInc,  // ++x
    PreDec,  // --x
    PostInc, // x++
    PostDec, // x--
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod, Pow, Concat,
    Eq, NotEq, Identical, NotIdentical, Spaceship,
    Lt, Le, Gt, Ge,
    BitAnd, BitOr, BitXor, Shl, Shr,
    BoolAnd, BoolOr, Coalesce,
    LogicalAnd, LogicalOr, LogicalXor, // the word forms `and`/`or`/`xor`
}

// --- types & attributes ---------------------------------------------------

#[derive(Clone, Debug)]
pub struct Type {
    pub span: Span,
    pub kind: TypeKind,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    /// `int`, `Foo`, `\A\B`, `self`, `static`, `array`, `callable`, `null`, ...
    Named(ByteString),
    /// `?T`
    Nullable(Box<Type>),
    /// `A|B` (each member may itself be an intersection for DNF types).
    Union(Vec<Type>),
    /// `A&B`
    Intersection(Vec<Type>),
}

#[derive(Clone, Debug)]
pub struct AttributeGroup {
    pub span: Span,
    pub attrs: Vec<Attribute>,
}

#[derive(Clone, Debug)]
pub struct Attribute {
    pub span: Span,
    pub name: ByteString,
    pub args: Vec<Arg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Public,
    Protected,
    Private,
    Static,
    Abstract,
    Final,
    Readonly,
    /// Asymmetric visibility (8.4): `public(set)` / `protected(set)` / `private(set)`.
    PublicSet,
    ProtectedSet,
    PrivateSet,
}

// --- declarations ---------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FunctionDecl {
    pub span: Span,
    pub attrs: Vec<AttributeGroup>,
    pub by_ref: bool,
    pub name: ByteString,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    /// `None` for a forward declaration / interface method (no body).
    pub body: Option<Vec<Stmt>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Interface,
    Trait,
    Enum,
}

#[derive(Clone, Debug)]
pub struct ClassLike {
    pub span: Span,
    pub attrs: Vec<AttributeGroup>,
    pub modifiers: Vec<Modifier>,
    pub kind: ClassKind,
    /// `None` for an anonymous class.
    pub name: Option<ByteString>,
    /// Backing type for `enum E: string`.
    pub enum_backing: Option<Type>,
    pub extends: Vec<ByteString>,
    pub implements: Vec<ByteString>,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub span: Span,
    pub attrs: Vec<AttributeGroup>,
    pub modifiers: Vec<Modifier>,
    pub kind: MemberKind,
}

#[derive(Clone, Debug)]
pub enum MemberKind {
    Property {
        ty: Option<Type>,
        /// One or more `$name = default`.
        props: Vec<(ByteString, Option<Expr>)>,
        hooks: Vec<PropertyHook>,
    },
    Const {
        ty: Option<Type>,
        consts: Vec<(ByteString, Expr)>,
    },
    Method(FunctionDecl),
    /// `case Name;` or `case Name = value;`
    EnumCase {
        name: ByteString,
        value: Option<Expr>,
    },
    /// `use A, B { ...adaptations... }`
    UseTrait {
        traits: Vec<ByteString>,
        adaptations: Vec<Span>,
    },
}

/// A property hook (8.4): `get => expr`, `get { ... }`, `set($v) { ... }`.
#[derive(Clone, Debug)]
pub struct PropertyHook {
    pub span: Span,
    pub by_ref: bool,
    /// `get` or `set`.
    pub name: ByteString,
    pub params: Vec<Param>,
    pub body: HookBody,
}

#[derive(Clone, Debug)]
pub enum HookBody {
    /// `=> expr;`
    Expr(Expr),
    /// `{ ... }`
    Block(Vec<Stmt>),
    /// Abstract hook in an interface: just `get;`.
    None,
}

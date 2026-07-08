//! A recursive-descent + Pratt parser producing the spanned AST in [`crate::ast`].
//!
//! Expressions use precedence climbing; the binding-power table encodes PHP 8.4's
//! operator precedence and associativity (including the 8.0 `.` change — concat
//! binds *looser* than `+`/`-`). Statements and declarations are only partially
//! modelled: anything not yet handled becomes [`StmtKind::Unknown`] with a span
//! covering the raw tokens, so the parser is total and never loses bytes.

use crate::ast::*;
use crate::bytestr::{ByteStr, ByteString};
use crate::lexer::lex;
use crate::span::Span;
use crate::token::{Keyword, Token, TokenKind};

#[derive(Clone, Debug)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

pub fn parse(src: impl AsRef<[u8]>) -> (Program, Vec<ParseError>) {
    let (program, errors, _) = parse_stats(src);
    (program, errors)
}

/// Like [`parse`], but also returns the number of `Unknown` recovery nodes — a
/// coverage metric for driving grammar completeness against a corpus.
pub fn parse_stats(src: impl AsRef<[u8]>) -> (Program, Vec<ParseError>, usize) {
    let mut p = Parser::new(src.as_ref());
    let mut stmts = Vec::new();
    while !p.at(TokenKind::Eof) {
        let before = p.pos;
        stmts.push(p.parse_stmt());
        if p.pos == before {
            p.bump(); // guarantee progress
        }
    }
    (Program { stmts }, p.errors, p.unknowns)
}

/// Parse a single expression from `src` (wrapped in `<?php`). Test/tooling entry.
pub fn parse_expr_str(src: &str) -> (Expr, Vec<ParseError>) {
    // The AST owns its data (Spans are plain offsets; names are copied into
    // `ByteString`), so nothing outlives the local buffer.
    let full = format!("<?php {src}");
    let mut p = Parser::new(full.as_bytes());
    p.eat(TokenKind::OpenTag);
    let e = p.parse_expr(0);
    (e, p.errors)
}

struct Parser<'a> {
    src: &'a [u8],
    toks: Vec<Token>,
    pos: usize,
    errors: Vec<ParseError>,
    /// Count of `Unknown` recovery nodes produced — a grammar-coverage metric.
    unknowns: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        let toks = lex(src).into_iter().filter(|t| !t.kind.is_trivia()).collect();
        Parser {
            src,
            toks,
            pos: 0,
            errors: Vec::new(),
            unknowns: 0,
        }
    }
}

// --- binding powers -------------------------------------------------------
//
// Higher binds tighter. Levels are doubled so left/right associativity can be
// expressed as a (left_bp, right_bp) pair without fractions.
const BP_ASSIGN: (u8, u8) = (9, 8); // right
const BP_TERNARY: u8 = 10;
const BP_COALESCE: (u8, u8) = (13, 12); // right
const BP_BANG_RIGHT: u8 = 35; // prefix `!` operand
const BP_UNARY_RIGHT: u8 = 38; // prefix `~ - + @ ++ -- (cast) clone`
const BP_INSTANCEOF: u8 = 36;
const BP_POSTFIX_MIN: u8 = 100; // call/index/member/:: always bind tightest

/// Infix binary operators: `(left_bp, right_bp, op)`.
fn binary_bp(kind: TokenKind) -> Option<(u8, u8, BinaryOp)> {
    use BinaryOp as B;
    use TokenKind as T;
    let (lvl_left, op) = match kind {
        T::Star => (34, B::Mul),
        T::Slash => (34, B::Div),
        T::Percent => (34, B::Mod),
        T::Plus => (32, B::Add),
        T::Minus => (32, B::Sub),
        T::Dot => (30, B::Concat),
        T::Shl => (28, B::Shl),
        T::Shr => (28, B::Shr),
        T::Lt => (26, B::Lt),
        T::Le => (26, B::Le),
        T::Gt => (26, B::Gt),
        T::Ge => (26, B::Ge),
        T::Eq => (24, B::Eq),
        T::NotEq => (24, B::NotEq),
        T::Identical => (24, B::Identical),
        T::NotIdentical => (24, B::NotIdentical),
        T::Spaceship => (24, B::Spaceship),
        T::Amp => (22, B::BitAnd),
        T::Caret => (20, B::BitXor),
        T::Pipe => (18, B::BitOr),
        T::BoolAnd => (16, B::BoolAnd),
        T::BoolOr => (14, B::BoolOr),
        T::Pow => return Some((41, 40, B::Pow)), // right-assoc
        T::Keyword(Keyword::And) => (6, B::LogicalAnd),
        T::Keyword(Keyword::Xor) => (4, B::LogicalXor),
        T::Keyword(Keyword::Or) => (2, B::LogicalOr),
        _ => return None,
    };
    Some((lvl_left, lvl_left + 1, op)) // left-assoc
}

impl<'a> Parser<'a> {
    // --- token helpers ----------------------------------------------------

    fn kind(&self) -> TokenKind {
        self.toks[self.pos].kind
    }

    fn kind_at(&self, n: usize) -> TokenKind {
        self.toks.get(self.pos + n).map(|t| t.kind).unwrap_or(TokenKind::Eof)
    }

    fn at(&self, k: TokenKind) -> bool {
        self.kind() == k
    }

    fn cur(&self) -> Token {
        self.toks[self.pos]
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos];
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: TokenKind, what: &str) {
        if !self.eat(k) {
            let span = self.cur().span;
            self.errors.push(ParseError {
                span,
                message: format!("expected {what}"),
            });
        }
    }

    /// A statement terminator. A `;` is consumed; a closing `?>` or EOF acts as an
    /// implicit terminator (PHP allows `<?= $x ?>` with no `;`) and is left for the
    /// next statement.
    fn expect_semi(&mut self) {
        if self.eat(TokenKind::Semicolon) {
            return;
        }
        if matches!(self.kind(), TokenKind::CloseTag | TokenKind::Eof) {
            return;
        }
        let span = self.cur().span;
        self.errors.push(ParseError { span, message: "expected `;`".into() });
    }

    fn lo(&self) -> usize {
        self.cur().span.start
    }

    fn prev_hi(&self) -> usize {
        self.toks[self.pos.saturating_sub(1)].span.end
    }

    fn span_from(&self, lo: usize) -> Span {
        // Clamp so a production that consumed nothing yields a zero-width span
        // rather than an inverted one (which would panic on slicing).
        Span::new(lo, self.prev_hi().max(lo))
    }

    fn text(&self, span: Span) -> ByteStr<'a> {
        span.slice(self.src)
    }

    // --- statements -------------------------------------------------------

    fn parse_stmt(&mut self) -> Stmt {
        let lo = self.lo();
        let attrs = self.parse_attributes();
        let kind = match self.kind() {
            TokenKind::OpenTag | TokenKind::OpenTagEcho => {
                self.bump();
                StmtKind::OpenTag
            }
            TokenKind::CloseTag => {
                self.bump();
                StmtKind::CloseTag
            }
            TokenKind::InlineHtml => {
                let s = self.cur().span;
                self.bump();
                StmtKind::InlineHtml(s)
            }
            TokenKind::Semicolon => {
                self.bump();
                StmtKind::Nop
            }
            TokenKind::LeftBrace => StmtKind::Block(self.parse_block()),
            TokenKind::Keyword(Keyword::Echo) => {
                self.bump();
                let mut exprs = vec![self.parse_expr(0)];
                while self.eat(TokenKind::Comma) {
                    exprs.push(self.parse_expr(0));
                }
                self.expect_semi();
                StmtKind::Echo(exprs)
            }
            TokenKind::Keyword(Keyword::Return) => {
                self.bump();
                let value = if self.at(TokenKind::Semicolon) {
                    None
                } else {
                    Some(self.parse_expr(0))
                };
                self.expect_semi();
                StmtKind::Return(value)
            }
            TokenKind::Keyword(Keyword::If) => return self.parse_if(),
            TokenKind::Keyword(Keyword::While) => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let cond = self.parse_expr(0);
                self.expect(TokenKind::RightParen, "`)`");
                let body = self.parse_body(Keyword::EndWhile);
                StmtKind::While { cond, body }
            }
            TokenKind::Keyword(Keyword::Do) => return self.parse_do_while(lo),
            TokenKind::Keyword(Keyword::For) => return self.parse_for(lo),
            TokenKind::Keyword(Keyword::Foreach) => return self.parse_foreach(lo),
            TokenKind::Keyword(Keyword::Switch) => return self.parse_switch(lo),
            TokenKind::Keyword(Keyword::Try) => return self.parse_try(lo),
            TokenKind::Keyword(Keyword::Break) => {
                self.bump();
                let level = if self.at(TokenKind::Semicolon) { None } else { Some(self.parse_expr(0)) };
                self.expect_semi();
                StmtKind::Break(level)
            }
            TokenKind::Keyword(Keyword::Continue) => {
                self.bump();
                let level = if self.at(TokenKind::Semicolon) { None } else { Some(self.parse_expr(0)) };
                self.expect_semi();
                StmtKind::Continue(level)
            }
            TokenKind::Keyword(Keyword::Goto) => {
                self.bump();
                let name = self.parse_member_ident();
                self.expect_semi();
                StmtKind::Goto(name)
            }
            TokenKind::Keyword(Keyword::Global) => {
                self.bump();
                let mut vars = Vec::new();
                while self.at(TokenKind::Variable) {
                    let s = self.cur().span;
                    self.bump();
                    vars.push(self.text(s).without_first().to_owned());
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_semi();
                StmtKind::Global(vars)
            }
            TokenKind::Keyword(Keyword::Unset) => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let mut items = Vec::new();
                while !self.at(TokenKind::RightParen) && !self.at(TokenKind::Eof) {
                    items.push(self.parse_expr(0));
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RightParen, "`)`");
                self.expect_semi();
                StmtKind::Unset(items)
            }
            // `static $x = 1;` (function static var). `static function/fn` is a closure.
            TokenKind::Keyword(Keyword::Static) if self.kind_at(1) == TokenKind::Variable => {
                self.bump();
                let mut vars = Vec::new();
                while self.at(TokenKind::Variable) {
                    let s = self.cur().span;
                    self.bump();
                    let name = self.text(s).without_first().to_owned();
                    let def = if self.eat(TokenKind::Assign) { Some(self.parse_expr(0)) } else { None };
                    vars.push((name, def));
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_semi();
                StmtKind::StaticVars(vars)
            }
            TokenKind::Keyword(Keyword::Const) => {
                self.bump();
                let mut consts = Vec::new();
                loop {
                    let name = self.parse_member_ident();
                    self.expect(TokenKind::Assign, "`=`");
                    let v = self.parse_expr(0);
                    consts.push((name, v));
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_semi();
                StmtKind::Const(consts)
            }
            TokenKind::Keyword(Keyword::Declare) => return self.parse_declare(lo),
            TokenKind::Keyword(Keyword::Namespace) => return self.parse_namespace(lo),
            TokenKind::Keyword(Keyword::Use) => self.parse_use(),
            // `function name(...)` is a declaration; `function(...)` is a closure expr.
            TokenKind::Keyword(Keyword::Function)
                if !matches!(self.kind_at(1), TokenKind::LeftParen)
                    && !(self.kind_at(1) == TokenKind::Amp
                        && self.kind_at(2) == TokenKind::LeftParen) =>
            {
                StmtKind::Function(self.parse_function(attrs))
            }
            TokenKind::Keyword(
                Keyword::Class
                | Keyword::Interface
                | Keyword::Trait
                | Keyword::Enum
                | Keyword::Abstract
                | Keyword::Final
                | Keyword::Readonly,
            ) => StmtKind::ClassLike(self.parse_classlike(attrs)),
            // A goto label: `identifier :` (but not `::`).
            TokenKind::Identifier if self.kind_at(1) == TokenKind::Colon => {
                let s = self.cur().span;
                self.bump();
                self.bump(); // :
                StmtKind::Label(self.text(s).to_owned())
            }
            _ => {
                let expr = self.parse_expr(0);
                self.expect_semi();
                StmtKind::Expr(expr)
            }
        };
        Stmt {
            span: self.span_from(lo),
            kind,
        }
    }

    fn parse_if(&mut self) -> Stmt {
        let lo = self.lo();
        self.bump(); // if
        self.expect(TokenKind::LeftParen, "`(`");
        let cond = self.parse_expr(0);
        self.expect(TokenKind::RightParen, "`)`");
        if self.at(TokenKind::Colon) {
            return self.parse_if_alt(lo, cond);
        }
        let then = self.parse_block_or_stmt();
        let mut else_ifs = Vec::new();
        let mut else_ = None;
        loop {
            match self.kind() {
                TokenKind::Keyword(Keyword::ElseIf) => {
                    self.bump();
                    self.expect(TokenKind::LeftParen, "`(`");
                    let c = self.parse_expr(0);
                    self.expect(TokenKind::RightParen, "`)`");
                    let b = self.parse_block_or_stmt();
                    else_ifs.push((c, b));
                }
                TokenKind::Keyword(Keyword::Else)
                    if self.kind_at(1) == TokenKind::Keyword(Keyword::If) =>
                {
                    self.bump(); // else
                    self.bump(); // if
                    self.expect(TokenKind::LeftParen, "`(`");
                    let c = self.parse_expr(0);
                    self.expect(TokenKind::RightParen, "`)`");
                    let b = self.parse_block_or_stmt();
                    else_ifs.push((c, b));
                }
                TokenKind::Keyword(Keyword::Else) => {
                    self.bump();
                    else_ = Some(self.parse_block_or_stmt());
                    break;
                }
                _ => break,
            }
        }
        Stmt {
            span: self.span_from(lo),
            kind: StmtKind::If {
                cond,
                then,
                else_ifs,
                else_,
            },
        }
    }

    /// Alternative-syntax `if (...): ... elseif (...): ... else: ... endif;`.
    fn parse_if_alt(&mut self, lo: usize, cond: Expr) -> Stmt {
        self.bump(); // :
        let then = self.parse_stmts_until_if_end();
        let mut else_ifs = Vec::new();
        let mut else_ = None;
        loop {
            match self.kind() {
                TokenKind::Keyword(Keyword::ElseIf) => {
                    self.bump();
                    self.expect(TokenKind::LeftParen, "`(`");
                    let c = self.parse_expr(0);
                    self.expect(TokenKind::RightParen, "`)`");
                    self.eat(TokenKind::Colon);
                    else_ifs.push((c, self.parse_stmts_until_if_end()));
                }
                TokenKind::Keyword(Keyword::Else) => {
                    self.bump();
                    self.eat(TokenKind::Colon);
                    else_ = Some(self.parse_stmts_until_if_end());
                    break;
                }
                _ => break,
            }
        }
        self.eat(TokenKind::Keyword(Keyword::EndIf));
        self.eat(TokenKind::Semicolon);
        Stmt {
            span: self.span_from(lo),
            kind: StmtKind::If { cond, then, else_ifs, else_ },
        }
    }

    fn parse_stmts_until_if_end(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        while !matches!(
            self.kind(),
            TokenKind::Keyword(Keyword::ElseIf)
                | TokenKind::Keyword(Keyword::Else)
                | TokenKind::Keyword(Keyword::EndIf)
                | TokenKind::Eof
        ) {
            let before = self.pos;
            stmts.push(self.parse_stmt());
            if self.pos == before {
                self.bump();
            }
        }
        stmts
    }

    fn parse_block(&mut self) -> Vec<Stmt> {
        self.expect(TokenKind::LeftBrace, "`{`");
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            stmts.push(self.parse_stmt());
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RightBrace, "`}`");
        stmts
    }

    fn parse_block_or_stmt(&mut self) -> Vec<Stmt> {
        if self.at(TokenKind::LeftBrace) {
            self.parse_block()
        } else {
            vec![self.parse_stmt()]
        }
    }

    /// Consume a construct we can't model, balancing brackets so a trailing `}`
    /// block or `;` ends it, with the span preserved for lossless emit. The full
    /// grammar now parses all of PHP 8.4, so this is unused — retained as an
    /// error-recovery fallback for Phase E.
    #[allow(dead_code)]
    fn parse_unknown_stmt(&mut self) -> Stmt {
        let lo = self.lo();
        let mut depth = 0i32;
        let mut saw_brace = false;
        loop {
            match self.kind() {
                TokenKind::Eof => break,
                // Interpolation openers (`{$…}` / `${…}`) count as braces so a
                // string like `"{$x}"` doesn't underflow the depth.
                TokenKind::LeftBrace | TokenKind::CurlyOpen | TokenKind::DollarOpenCurly => {
                    saw_brace = true;
                    depth += 1;
                    self.bump();
                }
                TokenKind::LeftParen | TokenKind::LeftBracket => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RightBrace => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                    if depth == 0 && saw_brace {
                        break;
                    }
                }
                TokenKind::RightParen | TokenKind::RightBracket => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.bump();
                }
                TokenKind::Semicolon => {
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
        let span = self.span_from(lo);
        self.unknowns += 1;
        Stmt {
            span,
            kind: StmtKind::Unknown(span),
        }
    }

    // --- expressions (Pratt) ---------------------------------------------

    fn parse_expr(&mut self, min_bp: u8) -> Expr {
        let mut lhs = self.parse_nud();
        loop {
            // Postfix operators bind tightest and always apply.
            match self.kind() {
                TokenKind::LeftParen => {
                    lhs = self.parse_call(lhs);
                    continue;
                }
                TokenKind::LeftBracket => {
                    lhs = self.parse_index(lhs);
                    continue;
                }
                TokenKind::Arrow | TokenKind::NullsafeArrow => {
                    lhs = self.parse_member(lhs);
                    continue;
                }
                TokenKind::DoubleColon => {
                    lhs = self.parse_static_member(lhs);
                    continue;
                }
                TokenKind::Inc | TokenKind::Dec if BP_POSTFIX_MIN >= min_bp => {
                    let op = if self.at(TokenKind::Inc) {
                        UnaryOp::PostInc
                    } else {
                        UnaryOp::PostDec
                    };
                    let lo = lhs.span.start;
                    self.bump();
                    lhs = Expr {
                        span: self.span_from(lo),
                        kind: ExprKind::PostfixIncDec {
                            op,
                            operand: Box::new(lhs),
                        },
                    };
                    continue;
                }
                _ => {}
            }

            // Ternary / coalesce / assignment / instanceof / binary.
            let k = self.kind();
            if k == TokenKind::Question {
                if BP_TERNARY < min_bp {
                    break;
                }
                lhs = self.parse_ternary(lhs);
                continue;
            }
            if k == TokenKind::Coalesce {
                if BP_COALESCE.0 < min_bp {
                    break;
                }
                let lo = lhs.span.start;
                self.bump();
                let rhs = self.parse_expr(BP_COALESCE.1);
                lhs = Expr {
                    span: self.span_from(lo),
                    kind: ExprKind::Binary {
                        op: BinaryOp::Coalesce,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                };
                continue;
            }
            if let Some(op) = assign_op(k) {
                if BP_ASSIGN.0 < min_bp {
                    break;
                }
                let lo = lhs.span.start;
                self.bump();
                let value = self.parse_expr(BP_ASSIGN.1);
                lhs = Expr {
                    span: self.span_from(lo),
                    kind: ExprKind::Assign {
                        op,
                        target: Box::new(lhs),
                        value: Box::new(value),
                    },
                };
                continue;
            }
            if k == TokenKind::Keyword(Keyword::InstanceOf) {
                if BP_INSTANCEOF < min_bp {
                    break;
                }
                let lo = lhs.span.start;
                self.bump();
                let class = self.parse_expr(BP_INSTANCEOF + 1);
                lhs = Expr {
                    span: self.span_from(lo),
                    kind: ExprKind::Instanceof {
                        expr: Box::new(lhs),
                        class: Box::new(class),
                    },
                };
                continue;
            }
            if let Some((l_bp, r_bp, op)) = binary_bp(k) {
                if l_bp < min_bp {
                    break;
                }
                let lo = lhs.span.start;
                self.bump();
                let rhs = self.parse_expr(r_bp);
                lhs = Expr {
                    span: self.span_from(lo),
                    kind: ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                };
                continue;
            }
            break;
        }
        lhs
    }

    fn parse_ternary(&mut self, cond: Expr) -> Expr {
        let lo = cond.span.start;
        self.bump(); // ?
        let then = if self.at(TokenKind::Colon) {
            None // short ternary `?:`
        } else {
            Some(Box::new(self.parse_expr(0)))
        };
        self.expect(TokenKind::Colon, "`:`");
        let else_ = self.parse_expr(BP_TERNARY);
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Ternary {
                cond: Box::new(cond),
                then,
                else_: Box::new(else_),
            },
        }
    }

    /// Null denotation: prefix operators and primary expressions.
    fn parse_nud(&mut self) -> Expr {
        let lo = self.lo();
        let kind = match self.kind() {
            TokenKind::Bang => {
                self.bump();
                ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(self.parse_expr(BP_BANG_RIGHT)),
                }
            }
            TokenKind::Minus => self.prefix(UnaryOp::Neg),
            TokenKind::Plus => self.prefix(UnaryOp::Pos),
            TokenKind::Tilde => self.prefix(UnaryOp::BitNot),
            TokenKind::At => self.prefix(UnaryOp::Suppress),
            // `&expr` — reference-of, e.g. `$a = &$b`.
            TokenKind::Amp => self.prefix(UnaryOp::Reference),
            // `$$var` / `${expr}` — variable variables.
            TokenKind::Dollar => {
                self.bump();
                let inner = if self.eat(TokenKind::LeftBrace) {
                    let e = self.parse_expr(0);
                    self.expect(TokenKind::RightBrace, "`}`");
                    e
                } else {
                    self.parse_nud()
                };
                ExprKind::VariableVariable(Box::new(inner))
            }
            TokenKind::Inc => self.prefix(UnaryOp::PreInc),
            TokenKind::Dec => self.prefix(UnaryOp::PreDec),
            TokenKind::Cast(c) => {
                self.bump();
                ExprKind::Cast {
                    cast: c,
                    operand: Box::new(self.parse_expr(BP_UNARY_RIGHT)),
                }
            }
            TokenKind::Keyword(Keyword::Clone) => {
                self.bump();
                ExprKind::Clone(Box::new(self.parse_expr(BP_UNARY_RIGHT)))
            }
            TokenKind::Keyword(Keyword::Print) => {
                self.bump();
                ExprKind::Print(Box::new(self.parse_expr(BP_ASSIGN.1)))
            }
            TokenKind::Keyword(Keyword::Throw) => {
                self.bump();
                ExprKind::Throw(Box::new(self.parse_expr(BP_ASSIGN.1)))
            }
            TokenKind::Keyword(Keyword::Yield) => {
                self.bump();
                if self.at_expr_start() {
                    let first = self.parse_expr(BP_ASSIGN.1);
                    if self.eat(TokenKind::DoubleArrow) {
                        let value = self.parse_expr(BP_ASSIGN.1);
                        ExprKind::Yield {
                            key: Some(Box::new(first)),
                            value: Some(Box::new(value)),
                        }
                    } else {
                        ExprKind::Yield {
                            key: None,
                            value: Some(Box::new(first)),
                        }
                    }
                } else {
                    ExprKind::Yield { key: None, value: None }
                }
            }
            TokenKind::Keyword(Keyword::YieldFrom) => {
                self.bump();
                ExprKind::YieldFrom(Box::new(self.parse_expr(BP_ASSIGN.1)))
            }
            TokenKind::Keyword(Keyword::New) => return self.parse_new(),
            TokenKind::Keyword(Keyword::Function) => return self.parse_closure(false),
            TokenKind::Keyword(Keyword::Static)
                if matches!(
                    self.kind_at(1),
                    TokenKind::Keyword(Keyword::Function) | TokenKind::Keyword(Keyword::Fn)
                ) =>
            {
                self.bump(); // static
                if self.at(TokenKind::Keyword(Keyword::Fn)) {
                    return self.parse_arrow_fn(true);
                }
                return self.parse_closure(true);
            }
            TokenKind::Keyword(Keyword::Fn) => return self.parse_arrow_fn(false),
            // `static` as a class reference (`static::foo`, `new static`).
            TokenKind::Keyword(Keyword::Static) => {
                let s = self.cur().span;
                self.bump();
                ExprKind::Name(self.text(s).to_owned())
            }
            TokenKind::Keyword(Keyword::Match) => return self.parse_match(),
            TokenKind::Keyword(Keyword::Include) => self.parse_include(IncludeKind::Include),
            TokenKind::Keyword(Keyword::IncludeOnce) => self.parse_include(IncludeKind::IncludeOnce),
            TokenKind::Keyword(Keyword::Require) => self.parse_include(IncludeKind::Require),
            TokenKind::Keyword(Keyword::RequireOnce) => self.parse_include(IncludeKind::RequireOnce),
            TokenKind::Keyword(Keyword::Isset) => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let mut items = vec![self.parse_expr(0)];
                while self.eat(TokenKind::Comma) && !self.at(TokenKind::RightParen) {
                    items.push(self.parse_expr(0));
                }
                self.expect(TokenKind::RightParen, "`)`");
                ExprKind::Isset(items)
            }
            TokenKind::Keyword(Keyword::Empty) => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let e = self.parse_expr(0);
                self.expect(TokenKind::RightParen, "`)`");
                ExprKind::Empty(Box::new(e))
            }
            TokenKind::Keyword(Keyword::List) if self.kind_at(1) == TokenKind::LeftParen => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let items = self.parse_array_items(TokenKind::RightParen);
                self.expect(TokenKind::RightParen, "`)`");
                ExprKind::List(items)
            }
            TokenKind::Keyword(Keyword::Array) if self.kind_at(1) == TokenKind::LeftParen => {
                self.bump();
                self.expect(TokenKind::LeftParen, "`(`");
                let items = self.parse_array_items(TokenKind::RightParen);
                self.expect(TokenKind::RightParen, "`)`");
                ExprKind::Array(items)
            }
            TokenKind::LeftBracket => {
                self.bump();
                let items = self.parse_array_items(TokenKind::RightBracket);
                self.expect(TokenKind::RightBracket, "`]`");
                ExprKind::Array(items)
            }
            TokenKind::LeftParen => {
                self.bump();
                let e = self.parse_expr(0);
                self.expect(TokenKind::RightParen, "`)`");
                // Preserve the parenthesized expression's own span via the inner node.
                return e;
            }
            TokenKind::Variable => {
                let s = self.cur().span;
                self.bump();
                ExprKind::Variable(self.text(s).without_first().to_owned())
            }
            TokenKind::Int => {
                self.bump();
                ExprKind::Int
            }
            TokenKind::Float => {
                self.bump();
                ExprKind::Float
            }
            TokenKind::ConstantString => {
                self.bump();
                ExprKind::String
            }
            TokenKind::DoubleQuote | TokenKind::Backtick | TokenKind::StartHeredoc => {
                return self.parse_interpolated();
            }
            TokenKind::Identifier | TokenKind::Backslash => {
                return self.parse_name();
            }
            TokenKind::MagicConstant => {
                let s = self.cur().span;
                self.bump();
                ExprKind::Name(self.text(s).to_owned())
            }
            // Any remaining keyword in expression position is a semi-reserved name
            // used as a constant / class reference (`Enum::cases()`, `Namespace\X`).
            TokenKind::Keyword(_) => return self.parse_name(),
            _ => {
                // Recover: emit an Error node covering one token.
                let span = self.cur().span;
                self.errors.push(ParseError {
                    span,
                    message: format!("unexpected token {:?}", self.kind()),
                });
                self.bump();
                ExprKind::Error
            }
        };
        Expr {
            span: self.span_from(lo),
            kind,
        }
    }

    fn prefix(&mut self, op: UnaryOp) -> ExprKind {
        self.bump();
        ExprKind::Unary {
            op,
            operand: Box::new(self.parse_expr(BP_UNARY_RIGHT)),
        }
    }

    fn at_expr_start(&self) -> bool {
        !matches!(
            self.kind(),
            TokenKind::Semicolon
                | TokenKind::RightParen
                | TokenKind::RightBracket
                | TokenKind::RightBrace
                | TokenKind::Comma
                | TokenKind::Eof
        )
    }

    fn parse_name(&mut self) -> Expr {
        let lo = self.lo();
        self.eat(TokenKind::Backslash);
        if self.at(TokenKind::Keyword(Keyword::Namespace)) {
            self.bump();
        }
        self.eat_name_segment();
        while self.at(TokenKind::Backslash) && self.name_segment_at(1) {
            self.bump(); // backslash
            self.eat_name_segment();
        }
        let span = self.span_from(lo);
        Expr {
            span,
            kind: ExprKind::Name(self.text(span).to_owned()),
        }
    }

    fn parse_interpolated(&mut self) -> Expr {
        let lo = self.lo();
        let open = self.bump().kind;
        let close = match open {
            TokenKind::DoubleQuote => TokenKind::DoubleQuote,
            TokenKind::Backtick => TokenKind::Backtick,
            _ => TokenKind::EndHeredoc,
        };
        let mut parts = Vec::new();
        loop {
            match self.kind() {
                k if k == close => {
                    self.bump();
                    break;
                }
                TokenKind::Eof => break,
                TokenKind::EncapsedText => {
                    parts.push(StringPart::Literal(self.cur().span));
                    self.bump();
                }
                TokenKind::Variable => {
                    let s = self.cur().span;
                    self.bump();
                    parts.push(StringPart::Expr(Expr {
                        span: s,
                        kind: ExprKind::Variable(self.text(s).without_first().to_owned()),
                    }));
                }
                TokenKind::CurlyOpen => {
                    self.bump();
                    let e = self.parse_expr(0);
                    self.expect(TokenKind::RightBrace, "`}`");
                    parts.push(StringPart::Expr(e));
                }
                TokenKind::DollarOpenCurly => {
                    self.bump();
                    if self.at(TokenKind::StringVarname) {
                        let s = self.cur().span;
                        self.bump();
                        parts.push(StringPart::Expr(Expr {
                            span: s,
                            kind: ExprKind::Variable(self.text(s).to_owned()),
                        }));
                    } else {
                        let e = self.parse_expr(0);
                        parts.push(StringPart::Expr(e));
                    }
                    self.expect(TokenKind::RightBrace, "`}`");
                }
                _ => {
                    self.bump();
                }
            }
        }
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Interpolated(parts),
        }
    }

    // --- postfix ----------------------------------------------------------

    fn parse_call(&mut self, callee: Expr) -> Expr {
        let lo = callee.span.start;
        // First-class callable syntax: `foo(...)`.
        if self.kind_at(1) == TokenKind::Ellipsis && self.kind_at(2) == TokenKind::RightParen {
            self.bump(); // (
            self.bump(); // ...
            self.bump(); // )
            return Expr {
                span: self.span_from(lo),
                kind: ExprKind::FirstClassCallable(Box::new(callee)),
            };
        }
        let args = self.parse_args();
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Call {
                callee: Box::new(callee),
                args,
            },
        }
    }

    fn parse_index(&mut self, base: Expr) -> Expr {
        let lo = base.span.start;
        self.bump(); // [
        let index = if self.at(TokenKind::RightBracket) {
            None
        } else {
            Some(Box::new(self.parse_expr(0)))
        };
        self.expect(TokenKind::RightBracket, "`]`");
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Index {
                base: Box::new(base),
                index,
            },
        }
    }

    fn parse_member(&mut self, object: Expr) -> Expr {
        let lo = object.span.start;
        let nullsafe = self.at(TokenKind::NullsafeArrow);
        self.bump(); // -> or ?->
        let name = self.parse_member_name();
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Member {
                object: Box::new(object),
                nullsafe,
                name,
            },
        }
    }

    fn parse_static_member(&mut self, class: Expr) -> Expr {
        let lo = class.span.start;
        self.bump(); // ::
        let name = self.parse_member_name();
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::StaticMember {
                class: Box::new(class),
                name,
            },
        }
    }

    fn parse_member_name(&mut self) -> MemberName {
        match self.kind() {
            TokenKind::Variable => {
                let s = self.cur().span;
                self.bump();
                MemberName::Variable(self.text(s).without_first().to_owned())
            }
            TokenKind::LeftBrace => {
                self.bump();
                let e = self.parse_expr(0);
                self.expect(TokenKind::RightBrace, "`}`");
                MemberName::Expr(Box::new(e))
            }
            // `::${$name}` / `::$$prop` — a dynamic member via a variable-variable.
            TokenKind::Dollar => MemberName::Expr(Box::new(self.parse_nud())),
            TokenKind::Keyword(Keyword::Class) => {
                // `Foo::class`
                let s = self.cur().span;
                self.bump();
                MemberName::Identifier(self.text(s).to_owned())
            }
            _ => {
                let s = self.cur().span;
                // Any identifier/keyword acts as a member name here.
                self.bump();
                MemberName::Identifier(self.text(s).to_owned())
            }
        }
    }

    fn parse_args(&mut self) -> Vec<Arg> {
        self.expect(TokenKind::LeftParen, "`(`");
        let mut args = Vec::new();
        while !self.at(TokenKind::RightParen) && !self.at(TokenKind::Eof) {
            let lo = self.lo();
            let spread = self.eat(TokenKind::Ellipsis);
            // Named argument — the name may be a keyword (`default: …`, `array: …`).
            let name = if matches!(self.kind(), TokenKind::Identifier | TokenKind::Keyword(_))
                && self.kind_at(1) == TokenKind::Colon
            {
                let s = self.cur().span;
                self.bump(); // name
                self.bump(); // :
                Some(self.text(s).to_owned())
            } else {
                None
            };
            let value = self.parse_expr(0);
            args.push(Arg {
                span: self.span_from(lo),
                name,
                value,
                spread,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RightParen, "`)`");
        args
    }

    // --- primaries with structure ----------------------------------------

    fn parse_array_items(&mut self, close: TokenKind) -> Vec<ArrayItem> {
        let mut items = Vec::new();
        while !self.at(close) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Comma) {
                // Skipped element in destructuring: `[, $b]`.
                let s = self.cur().span;
                self.bump();
                items.push(ArrayItem {
                    span: s,
                    key: None,
                    value: None,
                    by_ref: false,
                    spread: false,
                });
                continue;
            }
            let lo = self.lo();
            let spread = self.eat(TokenKind::Ellipsis);
            let by_ref = self.eat(TokenKind::Amp);
            let first = self.parse_expr(0);
            let (key, value) = if !spread && self.eat(TokenKind::DoubleArrow) {
                let by_ref2 = self.eat(TokenKind::Amp);
                let v = self.parse_expr(0);
                let _ = by_ref2;
                (Some(first), Some(v))
            } else {
                (None, Some(first))
            };
            items.push(ArrayItem {
                span: self.span_from(lo),
                key,
                value,
                by_ref,
                spread,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        items
    }

    /// After a modifier keyword at `new`, is there a `class` within reach (i.e.
    /// this is a modified anonymous class, not `new Readonly`)?
    fn anon_class_ahead(&self) -> bool {
        self.kind_at(1) == TokenKind::Keyword(Keyword::Class)
            || self.kind_at(2) == TokenKind::Keyword(Keyword::Class)
    }

    fn parse_new(&mut self) -> Expr {
        let lo = self.lo();
        self.bump(); // new
        // Anonymous class, optionally with modifiers: `new readonly class (...) { ... }`.
        if self.at(TokenKind::Keyword(Keyword::Class))
            || matches!(
                self.kind(),
                TokenKind::Keyword(Keyword::Readonly | Keyword::Abstract | Keyword::Final)
            ) && self.anon_class_ahead()
        {
            let clo = self.lo();
            let modifiers = self.parse_modifiers();
            self.eat(TokenKind::Keyword(Keyword::Class));
            let args = if self.at(TokenKind::LeftParen) { self.parse_args() } else { Vec::new() };
            let (extends, implements) = self.parse_class_heritage();
            let members = self.parse_class_body();
            let class = ClassLike {
                span: self.span_from(clo),
                attrs: Vec::new(),
                modifiers,
                kind: ClassKind::Class,
                name: None,
                enum_backing: None,
                extends,
                implements,
                members,
            };
            return Expr {
                span: self.span_from(lo),
                kind: ExprKind::NewAnon { args, class: Box::new(class) },
            };
        }
        let class = self.parse_new_target();
        let args = if self.at(TokenKind::LeftParen) {
            self.parse_args()
        } else {
            Vec::new()
        };
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::New {
                class: Box::new(class),
                args,
            },
        }
    }

    /// The class reference after `new`: a name, variable, or parenthesized expr,
    /// plus member/index chains — but *not* the trailing call parens (those are
    /// constructor arguments).
    fn parse_new_target(&mut self) -> Expr {
        let mut e = match self.kind() {
            TokenKind::Variable => {
                let s = self.cur().span;
                self.bump();
                Expr {
                    span: s,
                    kind: ExprKind::Variable(self.text(s).without_first().to_owned()),
                }
            }
            TokenKind::LeftParen => {
                self.bump();
                let inner = self.parse_expr(0);
                self.expect(TokenKind::RightParen, "`)`");
                inner
            }
            TokenKind::Keyword(Keyword::Static) => {
                let s = self.cur().span;
                self.bump();
                Expr {
                    span: s,
                    kind: ExprKind::Name(self.text(s).to_owned()),
                }
            }
            _ => self.parse_name(),
        };
        loop {
            match self.kind() {
                TokenKind::Arrow | TokenKind::NullsafeArrow => e = self.parse_member(e),
                TokenKind::DoubleColon => e = self.parse_static_member(e),
                TokenKind::LeftBracket => e = self.parse_index(e),
                _ => break,
            }
        }
        e
    }

    fn parse_match(&mut self) -> Expr {
        let lo = self.lo();
        self.bump(); // match
        self.expect(TokenKind::LeftParen, "`(`");
        let subject = self.parse_expr(0);
        self.expect(TokenKind::RightParen, "`)`");
        self.expect(TokenKind::LeftBrace, "`{`");
        let mut arms = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.at(TokenKind::Eof) {
            let arm_lo = self.lo();
            let conditions = if self.eat(TokenKind::Keyword(Keyword::Default)) {
                None
            } else {
                let mut conds = vec![self.parse_expr(0)];
                while self.eat(TokenKind::Comma) && !self.at(TokenKind::DoubleArrow) {
                    conds.push(self.parse_expr(0));
                }
                Some(conds)
            };
            self.expect(TokenKind::DoubleArrow, "`=>`");
            let body = self.parse_expr(0);
            arms.push(MatchArm {
                span: self.span_from(arm_lo),
                conditions,
                body,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RightBrace, "`}`");
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Match(Box::new(Match {
                span: self.span_from(lo),
                subject,
                arms,
            })),
        }
    }

    fn parse_closure(&mut self, is_static: bool) -> Expr {
        let lo = self.lo();
        self.bump(); // function
        let by_ref = self.eat(TokenKind::Amp);
        let params = self.parse_params();
        let mut uses = Vec::new();
        if self.eat(TokenKind::Keyword(Keyword::Use)) {
            self.expect(TokenKind::LeftParen, "`(`");
            while !self.at(TokenKind::RightParen) && !self.at(TokenKind::Eof) {
                let by_ref = self.eat(TokenKind::Amp);
                if self.at(TokenKind::Variable) {
                    let s = self.cur().span;
                    self.bump();
                    uses.push((self.text(s).without_first().to_owned(), by_ref));
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RightParen, "`)`");
        }
        self.skip_return_type();
        let body = self.parse_block();
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::Closure(Box::new(Closure {
                span: self.span_from(lo),
                is_static,
                by_ref,
                params,
                uses,
                body,
            })),
        }
    }

    fn parse_arrow_fn(&mut self, is_static: bool) -> Expr {
        let lo = self.lo();
        let fn_span = self.cur().span;
        self.bump(); // fn
        let by_ref = self.eat(TokenKind::Amp);
        let params = self.parse_params();
        let params_end = self.prev_hi(); // just after `)` (before any return type)
        self.skip_return_type();
        let arrow_span = self.cur().span;
        self.expect(TokenKind::DoubleArrow, "`=>`");
        // pxp extension: a `{ ... }` body makes this a block-bodied short closure.
        if self.at(TokenKind::LeftBrace) {
            let body = self.parse_block();
            return Expr {
                span: self.span_from(lo),
                kind: ExprKind::BlockArrowFn(Box::new(BlockArrowFn {
                    span: self.span_from(lo),
                    is_static,
                    by_ref,
                    params,
                    body,
                    fn_span,
                    arrow_span,
                    params_end,
                })),
            };
        }
        let body = self.parse_expr(0);
        Expr {
            span: self.span_from(lo),
            kind: ExprKind::ArrowFn(Box::new(ArrowFn {
                span: self.span_from(lo),
                is_static,
                by_ref,
                params,
                body,
            })),
        }
    }

    fn parse_params(&mut self) -> Vec<Param> {
        self.expect(TokenKind::LeftParen, "`(`");
        let mut params = Vec::new();
        while !self.at(TokenKind::RightParen) && !self.at(TokenKind::Eof) {
            let lo = self.lo();
            let attrs = self.parse_attributes();
            let modifiers = self.parse_modifiers();
            let ty = if self.at_type_start() { Some(self.parse_type()) } else { None };
            let by_ref = self.eat(TokenKind::Amp);
            let variadic = self.eat(TokenKind::Ellipsis);
            let name = if self.at(TokenKind::Variable) {
                let s = self.cur().span;
                self.bump();
                self.text(s).without_first().to_owned()
            } else {
                ByteString::new()
            };
            let default = if self.eat(TokenKind::Assign) {
                Some(self.parse_expr(0))
            } else {
                None
            };
            let hooks = if self.at(TokenKind::LeftBrace) {
                self.parse_property_hooks()
            } else {
                Vec::new()
            };
            params.push(Param {
                span: self.span_from(lo),
                attrs,
                modifiers,
                ty,
                by_ref,
                variadic,
                name,
                default,
                hooks,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RightParen, "`)`");
        params
    }

    fn skip_return_type(&mut self) {
        if self.eat(TokenKind::Colon) {
            while !self.at(TokenKind::LeftBrace)
                && !self.at(TokenKind::DoubleArrow)
                && !self.at(TokenKind::Semicolon)
                && !self.at(TokenKind::Eof)
            {
                self.bump();
            }
        }
    }

    fn parse_include(&mut self, kind: IncludeKind) -> ExprKind {
        self.bump();
        ExprKind::Include {
            kind,
            path: Box::new(self.parse_expr(0)),
        }
    }

    // --- attributes -------------------------------------------------------

    fn parse_attributes(&mut self) -> Vec<AttributeGroup> {
        let mut groups = Vec::new();
        while self.at(TokenKind::Attribute) {
            let lo = self.lo();
            self.bump(); // #[
            let mut attrs = Vec::new();
            while !self.at(TokenKind::RightBracket) && !self.at(TokenKind::Eof) {
                let alo = self.lo();
                let name = self.parse_name_string();
                let args = if self.at(TokenKind::LeftParen) {
                    self.parse_args()
                } else {
                    Vec::new()
                };
                attrs.push(Attribute { span: self.span_from(alo), name, args });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RightBracket, "`]`");
            groups.push(AttributeGroup { span: self.span_from(lo), attrs });
        }
        groups
    }

    // --- types ------------------------------------------------------------

    fn at_type_start(&self) -> bool {
        // Only called in type positions (param/property/const types), where a
        // keyword can only be a (semi-reserved) type name like `Enum` or `array`.
        matches!(
            self.kind(),
            TokenKind::Question
                | TokenKind::Backslash
                | TokenKind::Identifier
                | TokenKind::LeftParen // DNF `(A&B)`
                | TokenKind::Keyword(_)
        )
    }

    fn parse_type(&mut self) -> Type {
        let lo = self.lo();
        let first = self.parse_type_atom();
        if self.at(TokenKind::Pipe) {
            let mut members = vec![first];
            while self.eat(TokenKind::Pipe) {
                members.push(self.parse_type_atom());
            }
            Type { span: self.span_from(lo), kind: TypeKind::Union(members) }
        } else if self.at(TokenKind::Amp) && self.is_intersection_amp() {
            let mut members = vec![first];
            while self.at(TokenKind::Amp) && self.is_intersection_amp() {
                self.bump();
                members.push(self.parse_type_atom());
            }
            Type { span: self.span_from(lo), kind: TypeKind::Intersection(members) }
        } else {
            first
        }
    }

    fn parse_type_atom(&mut self) -> Type {
        let lo = self.lo();
        if self.eat(TokenKind::Question) {
            let inner = self.parse_type_atom();
            return Type { span: self.span_from(lo), kind: TypeKind::Nullable(Box::new(inner)) };
        }
        if self.eat(TokenKind::LeftParen) {
            let mut members = vec![self.parse_type_name()];
            while self.eat(TokenKind::Amp) {
                members.push(self.parse_type_name());
            }
            self.expect(TokenKind::RightParen, "`)`");
            return Type { span: self.span_from(lo), kind: TypeKind::Intersection(members) };
        }
        self.parse_type_name()
    }

    fn parse_type_name(&mut self) -> Type {
        let lo = self.lo();
        let name = match self.kind() {
            TokenKind::Keyword(Keyword::Array) => {
                self.bump();
                ByteString::from("array")
            }
            TokenKind::Keyword(Keyword::Callable) => {
                self.bump();
                ByteString::from("callable")
            }
            TokenKind::Keyword(Keyword::Static) => {
                self.bump();
                ByteString::from("static")
            }
            _ => self.parse_name_string(),
        };
        Type { span: self.span_from(lo), kind: TypeKind::Named(name) }
    }

    /// A `&` beginning an intersection type (followed by a type name), as opposed
    /// to a by-reference marker (`&$var`).
    fn is_intersection_amp(&self) -> bool {
        matches!(
            self.kind_at(1),
            TokenKind::Identifier
                | TokenKind::Backslash
                | TokenKind::Keyword(Keyword::Array)
                | TokenKind::Keyword(Keyword::Callable)
                | TokenKind::Keyword(Keyword::Static)
                | TokenKind::Keyword(Keyword::Namespace)
        )
    }

    // --- names & modifiers ------------------------------------------------

    /// Consume a (possibly qualified) name and return its raw text. Name segments
    /// may be keywords (`Foo\Array\List` is a valid name — keywords are only
    /// semi-reserved).
    fn parse_name_string(&mut self) -> ByteString {
        let lo = self.lo();
        self.eat(TokenKind::Backslash);
        self.eat_name_segment();
        while self.at(TokenKind::Backslash) && self.name_segment_at(1) {
            self.bump(); // backslash
            self.eat_name_segment();
        }
        let span = self.span_from(lo);
        self.text(span).to_owned()
    }

    fn eat_name_segment(&mut self) {
        if matches!(self.kind(), TokenKind::Identifier | TokenKind::Keyword(_)) {
            self.bump();
        }
    }

    fn name_segment_at(&self, n: usize) -> bool {
        matches!(self.kind_at(n), TokenKind::Identifier | TokenKind::Keyword(_))
    }

    /// A single identifier/keyword used as a name (member, const, label, alias).
    fn parse_member_ident(&mut self) -> ByteString {
        let s = self.cur().span;
        self.bump();
        self.text(s).to_owned()
    }

    fn parse_modifiers(&mut self) -> Vec<Modifier> {
        let mut mods = Vec::new();
        loop {
            let m = match self.kind() {
                TokenKind::Keyword(Keyword::Public) => {
                    self.bump();
                    if self.eat_set_paren() { Modifier::PublicSet } else { Modifier::Public }
                }
                TokenKind::Keyword(Keyword::Protected) => {
                    self.bump();
                    if self.eat_set_paren() { Modifier::ProtectedSet } else { Modifier::Protected }
                }
                TokenKind::Keyword(Keyword::Private) => {
                    self.bump();
                    if self.eat_set_paren() { Modifier::PrivateSet } else { Modifier::Private }
                }
                TokenKind::Keyword(Keyword::Static) => {
                    self.bump();
                    Modifier::Static
                }
                TokenKind::Keyword(Keyword::Abstract) => {
                    self.bump();
                    Modifier::Abstract
                }
                TokenKind::Keyword(Keyword::Final) => {
                    self.bump();
                    Modifier::Final
                }
                TokenKind::Keyword(Keyword::Readonly) => {
                    self.bump();
                    Modifier::Readonly
                }
                TokenKind::Keyword(Keyword::Var) => {
                    self.bump();
                    Modifier::Public
                }
                _ => break,
            };
            mods.push(m);
        }
        mods
    }

    /// The `(set)` of an asymmetric-visibility modifier.
    fn eat_set_paren(&mut self) -> bool {
        if self.at(TokenKind::LeftParen)
            && self.kind_at(1) == TokenKind::Identifier
            && self.kind_at(2) == TokenKind::RightParen
        {
            self.bump();
            self.bump();
            self.bump();
            true
        } else {
            false
        }
    }

    // --- declarations -----------------------------------------------------

    fn parse_function(&mut self, attrs: Vec<AttributeGroup>) -> FunctionDecl {
        let lo = self.lo();
        self.bump(); // function
        let by_ref = self.eat(TokenKind::Amp);
        let name = self.parse_member_ident();
        let params = self.parse_params();
        let return_type = if self.eat(TokenKind::Colon) { Some(self.parse_type()) } else { None };
        let body = if self.at(TokenKind::LeftBrace) {
            Some(self.parse_block())
        } else {
            self.eat(TokenKind::Semicolon);
            None
        };
        FunctionDecl { span: self.span_from(lo), attrs, by_ref, name, params, return_type, body }
    }

    fn parse_class_heritage(&mut self) -> (Vec<ByteString>, Vec<ByteString>) {
        let mut extends = Vec::new();
        if self.eat(TokenKind::Keyword(Keyword::Extends)) {
            extends.push(self.parse_name_string());
            while self.eat(TokenKind::Comma) {
                extends.push(self.parse_name_string());
            }
        }
        let mut implements = Vec::new();
        if self.eat(TokenKind::Keyword(Keyword::Implements)) {
            implements.push(self.parse_name_string());
            while self.eat(TokenKind::Comma) {
                implements.push(self.parse_name_string());
            }
        }
        (extends, implements)
    }

    fn parse_classlike(&mut self, attrs: Vec<AttributeGroup>) -> ClassLike {
        let lo = self.lo();
        let modifiers = self.parse_modifiers();
        let kind = match self.kind() {
            TokenKind::Keyword(Keyword::Interface) => ClassKind::Interface,
            TokenKind::Keyword(Keyword::Trait) => ClassKind::Trait,
            TokenKind::Keyword(Keyword::Enum) => ClassKind::Enum,
            _ => ClassKind::Class,
        };
        self.bump(); // class / interface / trait / enum
        let name = Some(self.parse_member_ident());
        let enum_backing = if kind == ClassKind::Enum && self.eat(TokenKind::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let (extends, implements) = self.parse_class_heritage();
        let members = self.parse_class_body();
        ClassLike {
            span: self.span_from(lo),
            attrs,
            modifiers,
            kind,
            name,
            enum_backing,
            extends,
            implements,
            members,
        }
    }

    fn parse_class_body(&mut self) -> Vec<Member> {
        self.expect(TokenKind::LeftBrace, "`{`");
        let mut members = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            members.push(self.parse_class_member());
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RightBrace, "`}`");
        members
    }

    fn parse_class_member(&mut self) -> Member {
        let lo = self.lo();
        let attrs = self.parse_attributes();

        if self.at(TokenKind::Keyword(Keyword::Use)) {
            self.bump();
            let mut traits = vec![self.parse_name_string()];
            while self.eat(TokenKind::Comma) {
                traits.push(self.parse_name_string());
            }
            let mut adaptations = Vec::new();
            if self.at(TokenKind::LeftBrace) {
                let alo = self.lo();
                self.skip_balanced_braces();
                adaptations.push(self.span_from(alo));
            } else {
                self.eat(TokenKind::Semicolon);
            }
            return Member {
                span: self.span_from(lo),
                attrs,
                modifiers: Vec::new(),
                kind: MemberKind::UseTrait { traits, adaptations },
            };
        }

        if self.at(TokenKind::Keyword(Keyword::Case)) {
            self.bump();
            let name = self.parse_member_ident();
            let value = if self.eat(TokenKind::Assign) { Some(self.parse_expr(0)) } else { None };
            self.expect_semi();
            return Member {
                span: self.span_from(lo),
                attrs,
                modifiers: Vec::new(),
                kind: MemberKind::EnumCase { name, value },
            };
        }

        let modifiers = self.parse_modifiers();

        if self.at(TokenKind::Keyword(Keyword::Const)) {
            self.bump();
            // Typed constant (8.3): a type precedes the name, unless the name (which
            // may be a keyword like `ARRAY`/`NAMESPACE`) is directly followed by `=`.
            let untyped = matches!(self.kind(), TokenKind::Identifier | TokenKind::Keyword(_))
                && self.kind_at(1) == TokenKind::Assign;
            let ty = if self.at_type_start() && !untyped {
                Some(self.parse_type())
            } else {
                None
            };
            let mut consts = Vec::new();
            loop {
                let name = self.parse_member_ident();
                self.expect(TokenKind::Assign, "`=`");
                let v = self.parse_expr(0);
                consts.push((name, v));
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect_semi();
            return Member {
                span: self.span_from(lo),
                attrs,
                modifiers,
                kind: MemberKind::Const { ty, consts },
            };
        }

        if self.at(TokenKind::Keyword(Keyword::Function)) {
            let f = self.parse_function(Vec::new());
            return Member {
                span: self.span_from(lo),
                attrs,
                modifiers,
                kind: MemberKind::Method(f),
            };
        }

        // Property: `[type] $a = 1, $b { hooks } ;`
        let ty = if self.at_type_start() { Some(self.parse_type()) } else { None };
        let mut props = Vec::new();
        while self.at(TokenKind::Variable) {
            let s = self.cur().span;
            self.bump();
            let name = self.text(s).without_first().to_owned();
            let def = if self.eat(TokenKind::Assign) { Some(self.parse_expr(0)) } else { None };
            props.push((name, def));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let hooks = if self.at(TokenKind::LeftBrace) {
            self.parse_property_hooks()
        } else {
            Vec::new()
        };
        self.eat(TokenKind::Semicolon);
        Member {
            span: self.span_from(lo),
            attrs,
            modifiers,
            kind: MemberKind::Property { ty, props, hooks },
        }
    }

    fn parse_property_hooks(&mut self) -> Vec<PropertyHook> {
        self.expect(TokenKind::LeftBrace, "`{`");
        let mut hooks = Vec::new();
        while !self.at(TokenKind::RightBrace) && !self.at(TokenKind::Eof) {
            let lo = self.lo();
            let _attrs = self.parse_attributes();
            let _mods = self.parse_modifiers();
            let by_ref = self.eat(TokenKind::Amp);
            if !matches!(self.kind(), TokenKind::Identifier) {
                self.bump(); // recover
                continue;
            }
            let name = self.parse_member_ident();
            let params = if self.at(TokenKind::LeftParen) { self.parse_params() } else { Vec::new() };
            let body = if self.eat(TokenKind::DoubleArrow) {
                let e = self.parse_expr(0);
                self.expect_semi();
                HookBody::Expr(e)
            } else if self.at(TokenKind::LeftBrace) {
                HookBody::Block(self.parse_block())
            } else {
                self.expect_semi();
                HookBody::None
            };
            hooks.push(PropertyHook { span: self.span_from(lo), by_ref, name, params, body });
        }
        self.expect(TokenKind::RightBrace, "`}`");
        hooks
    }

    fn skip_balanced_braces(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                TokenKind::Eof => break,
                TokenKind::LeftBrace | TokenKind::CurlyOpen | TokenKind::DollarOpenCurly => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RightBrace => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    // --- control flow -----------------------------------------------------

    /// A statement body, supporting both `{ ... }` and the alternative
    /// `: ... end<kw>;` syntax.
    fn parse_body(&mut self, end: Keyword) -> Vec<Stmt> {
        if self.eat(TokenKind::Colon) {
            let mut stmts = Vec::new();
            while !self.at(TokenKind::Keyword(end)) && !self.at(TokenKind::Eof) {
                let before = self.pos;
                stmts.push(self.parse_stmt());
                if self.pos == before {
                    self.bump();
                }
            }
            self.eat(TokenKind::Keyword(end));
            self.eat(TokenKind::Semicolon);
            stmts
        } else {
            self.parse_block_or_stmt()
        }
    }

    fn parse_expr_list(&mut self, term: TokenKind) -> Vec<Expr> {
        let mut exprs = Vec::new();
        if self.at(term) {
            return exprs;
        }
        loop {
            exprs.push(self.parse_expr(0));
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        exprs
    }

    fn parse_do_while(&mut self, lo: usize) -> Stmt {
        self.bump(); // do
        let body = self.parse_block_or_stmt();
        self.expect(TokenKind::Keyword(Keyword::While), "`while`");
        self.expect(TokenKind::LeftParen, "`(`");
        let cond = self.parse_expr(0);
        self.expect(TokenKind::RightParen, "`)`");
        self.expect_semi();
        Stmt { span: self.span_from(lo), kind: StmtKind::DoWhile { body, cond } }
    }

    fn parse_for(&mut self, lo: usize) -> Stmt {
        self.bump();
        self.expect(TokenKind::LeftParen, "`(`");
        let init = self.parse_expr_list(TokenKind::Semicolon);
        self.expect_semi();
        let cond = self.parse_expr_list(TokenKind::Semicolon);
        self.expect_semi();
        let update = self.parse_expr_list(TokenKind::RightParen);
        self.expect(TokenKind::RightParen, "`)`");
        let body = self.parse_body(Keyword::EndFor);
        Stmt { span: self.span_from(lo), kind: StmtKind::For { init, cond, update, body } }
    }

    fn parse_foreach(&mut self, lo: usize) -> Stmt {
        self.bump();
        self.expect(TokenKind::LeftParen, "`(`");
        let subject = self.parse_expr(0);
        self.expect(TokenKind::Keyword(Keyword::As), "`as`");
        let by_ref1 = self.eat(TokenKind::Amp);
        let first = self.parse_expr(0);
        let (key, by_ref, value) = if self.eat(TokenKind::DoubleArrow) {
            let by_ref2 = self.eat(TokenKind::Amp);
            let v = self.parse_expr(0);
            (Some(first), by_ref2, v)
        } else {
            (None, by_ref1, first)
        };
        self.expect(TokenKind::RightParen, "`)`");
        let body = self.parse_body(Keyword::EndForeach);
        Stmt {
            span: self.span_from(lo),
            kind: StmtKind::Foreach { subject, key, by_ref, value, body },
        }
    }

    fn parse_switch(&mut self, lo: usize) -> Stmt {
        self.bump();
        self.expect(TokenKind::LeftParen, "`(`");
        let subject = self.parse_expr(0);
        self.expect(TokenKind::RightParen, "`)`");
        let alt = self.eat(TokenKind::Colon);
        if !alt {
            self.expect(TokenKind::LeftBrace, "`{`");
        }
        let mut cases = Vec::new();
        while !self.at(TokenKind::RightBrace)
            && !self.at(TokenKind::Keyword(Keyword::EndSwitch))
            && !self.at(TokenKind::Eof)
        {
            let clo = self.lo();
            let test = if self.eat(TokenKind::Keyword(Keyword::Case)) {
                Some(self.parse_expr(0))
            } else if self.eat(TokenKind::Keyword(Keyword::Default)) {
                None
            } else {
                self.bump(); // recover
                continue;
            };
            if !self.eat(TokenKind::Colon) {
                self.eat(TokenKind::Semicolon);
            }
            let mut body = Vec::new();
            while !matches!(
                self.kind(),
                TokenKind::Keyword(Keyword::Case)
                    | TokenKind::Keyword(Keyword::Default)
                    | TokenKind::RightBrace
                    | TokenKind::Keyword(Keyword::EndSwitch)
                    | TokenKind::Eof
            ) {
                let before = self.pos;
                body.push(self.parse_stmt());
                if self.pos == before {
                    self.bump();
                }
            }
            cases.push(SwitchCase { span: self.span_from(clo), test, body });
        }
        if alt {
            self.eat(TokenKind::Keyword(Keyword::EndSwitch));
            self.eat(TokenKind::Semicolon);
        } else {
            self.expect(TokenKind::RightBrace, "`}`");
        }
        Stmt { span: self.span_from(lo), kind: StmtKind::Switch { subject, cases } }
    }

    fn parse_try(&mut self, lo: usize) -> Stmt {
        self.bump();
        let body = self.parse_block();
        let mut catches = Vec::new();
        while self.at(TokenKind::Keyword(Keyword::Catch)) {
            let clo = self.lo();
            self.bump();
            self.expect(TokenKind::LeftParen, "`(`");
            let mut types = vec![self.parse_type_name()];
            while self.eat(TokenKind::Pipe) {
                types.push(self.parse_type_name());
            }
            let var = if self.at(TokenKind::Variable) {
                let s = self.cur().span;
                self.bump();
                Some(self.text(s).without_first().to_owned())
            } else {
                None
            };
            self.expect(TokenKind::RightParen, "`)`");
            let cbody = self.parse_block();
            catches.push(Catch { span: self.span_from(clo), types, var, body: cbody });
        }
        let finally = if self.eat(TokenKind::Keyword(Keyword::Finally)) {
            Some(self.parse_block())
        } else {
            None
        };
        Stmt { span: self.span_from(lo), kind: StmtKind::Try { body, catches, finally } }
    }

    fn parse_declare(&mut self, lo: usize) -> Stmt {
        self.bump();
        self.expect(TokenKind::LeftParen, "`(`");
        let dlo = self.lo();
        while !self.at(TokenKind::RightParen) && !self.at(TokenKind::Eof) {
            self.bump();
        }
        let directives = self.span_from(dlo);
        self.expect(TokenKind::RightParen, "`)`");
        let body = if self.at(TokenKind::LeftBrace) {
            Some(self.parse_block())
        } else {
            self.eat(TokenKind::Semicolon);
            None
        };
        Stmt { span: self.span_from(lo), kind: StmtKind::Declare { directives, body } }
    }

    fn parse_namespace(&mut self, lo: usize) -> Stmt {
        self.bump();
        let name = if self.at(TokenKind::Identifier) || self.at(TokenKind::Backslash) {
            Some(self.parse_name_string())
        } else {
            None
        };
        let body = if self.at(TokenKind::LeftBrace) {
            Some(self.parse_block())
        } else {
            self.eat(TokenKind::Semicolon);
            None
        };
        Stmt { span: self.span_from(lo), kind: StmtKind::Namespace { name, body } }
    }

    fn parse_use(&mut self) -> StmtKind {
        self.bump();
        let kind = if self.eat(TokenKind::Keyword(Keyword::Function)) {
            UseKind::Function
        } else if self.eat(TokenKind::Keyword(Keyword::Const)) {
            UseKind::Const
        } else {
            UseKind::Normal
        };
        let mut items = Vec::new();
        loop {
            let ilo = self.lo();
            let path = self.parse_name_string();
            // Grouped use: `A\B\{ ... }` — the `\` precedes the group brace.
            if self.at(TokenKind::Backslash) && self.kind_at(1) == TokenKind::LeftBrace {
                self.bump();
            }
            if self.at(TokenKind::LeftBrace) {
                self.bump();
                while !self.at(TokenKind::RightBrace) && !self.at(TokenKind::Eof) {
                    let glo = self.lo();
                    let subkind = if self.eat(TokenKind::Keyword(Keyword::Function)) {
                        Some(UseKind::Function)
                    } else if self.eat(TokenKind::Keyword(Keyword::Const)) {
                        Some(UseKind::Const)
                    } else {
                        None
                    };
                    let sub = self.parse_name_string();
                    let alias = if self.eat(TokenKind::Keyword(Keyword::As)) {
                        Some(self.parse_member_ident())
                    } else {
                        None
                    };
                    // Byte concatenation of prefix + item (not Display — bytes).
                    let mut full = path.clone();
                    full.push_bytes(sub.as_bytes());
                    items.push(UseItem {
                        span: self.span_from(glo),
                        path: full,
                        alias,
                        kind: subkind,
                    });
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RightBrace, "`}`");
            } else {
                let alias = if self.eat(TokenKind::Keyword(Keyword::As)) {
                    Some(self.parse_member_ident())
                } else {
                    None
                };
                items.push(UseItem { span: self.span_from(ilo), path, alias, kind: None });
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect_semi();
        StmtKind::Use(UseDecl { kind, items })
    }
}

fn assign_op(k: TokenKind) -> Option<Option<BinaryOp>> {
    use BinaryOp as B;
    use TokenKind as T;
    Some(match k {
        T::Assign => None,
        T::PlusAssign => Some(B::Add),
        T::MinusAssign => Some(B::Sub),
        T::StarAssign => Some(B::Mul),
        T::SlashAssign => Some(B::Div),
        T::PercentAssign => Some(B::Mod),
        T::PowAssign => Some(B::Pow),
        T::DotAssign => Some(B::Concat),
        T::AmpAssign => Some(B::BitAnd),
        T::PipeAssign => Some(B::BitOr),
        T::CaretAssign => Some(B::BitXor),
        T::ShlAssign => Some(B::Shl),
        T::ShrAssign => Some(B::Shr),
        T::CoalesceAssign => Some(B::Coalesce),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse, ParseError};
    use crate::ast::*;
    use crate::span::Span;
    use crate::token::TokenKind;

    /// Parse a single expression, returning the owned source (so spans resolve)
    /// and the expression.
    fn parse_e(src: &str) -> (String, Expr) {
        let full = format!("<?php {src}");
        let mut p = super::Parser::new(full.as_bytes());
        p.eat(TokenKind::OpenTag);
        let e = p.parse_expr(0);
        assert!(p.errors.is_empty(), "unexpected parse errors for {src:?}: {:?}", p.errors);
        (full, e)
    }

    /// Render an expression to an S-expression so precedence is easy to assert.
    fn sexpr(src: &str, e: &Expr) -> String {
        use ExprKind::*;
        let lit = |sp: Span| sp.slice(src.as_bytes()).to_str_lossy().into_owned();
        match &e.kind {
            Int | Float | String => lit(e.span),
            Variable(n) => format!("${n}"),
            Name(n) => n.to_str_lossy().into_owned(),
            Unary { op, operand } => format!("({} {})", unop(*op), sexpr(src, operand)),
            PostfixIncDec { op, operand } => format!("({} {})", unop(*op), sexpr(src, operand)),
            Binary { op, lhs, rhs } => {
                format!("({} {} {})", binop(*op), sexpr(src, lhs), sexpr(src, rhs))
            }
            Assign { op, target, value } => {
                let o = op.map(binop).unwrap_or("=");
                format!("({} {} {})", o, sexpr(src, target), sexpr(src, value))
            }
            Ternary { cond, then, else_ } => format!(
                "(?: {} {} {})",
                sexpr(src, cond),
                then.as_ref().map(|t| sexpr(src, t)).unwrap_or_else(|| "_".into()),
                sexpr(src, else_)
            ),
            Cast { operand, .. } => format!("(cast {})", sexpr(src, operand)),
            Instanceof { expr, class } => {
                format!("(instanceof {} {})", sexpr(src, expr), sexpr(src, class))
            }
            Call { callee, args } => {
                let a: Vec<_> = args.iter().map(|x| sexpr(src, &x.value)).collect();
                format!("(call {} [{}])", sexpr(src, callee), a.join(" "))
            }
            Member { object, nullsafe, name } => format!(
                "({} {} {})",
                if *nullsafe { "?->" } else { "->" },
                sexpr(src, object),
                member(name)
            ),
            StaticMember { class, name } => {
                format!("(:: {} {})", sexpr(src, class), member(name))
            }
            Index { base, index } => format!(
                "([] {} {})",
                sexpr(src, base),
                index.as_ref().map(|i| sexpr(src, i)).unwrap_or_else(|| "_".into())
            ),
            New { class, args } => {
                let a: Vec<_> = args.iter().map(|x| sexpr(src, &x.value)).collect();
                format!("(new {} [{}])", sexpr(src, class), a.join(" "))
            }
            ArrowFn(f) => format!("(fn => {})", sexpr(src, &f.body)),
            BlockArrowFn(_) => "(block-fn)".into(),
            Closure(_) => "(closure)".into(),
            Match(m) => format!("(match {})", sexpr(src, &m.subject)),
            Array(items) => {
                let a: Vec<_> = items
                    .iter()
                    .map(|it| match (&it.key, &it.value) {
                        (Some(k), Some(v)) => format!("{}=>{}", sexpr(src, k), sexpr(src, v)),
                        (None, Some(v)) => sexpr(src, v),
                        _ => "_".into(),
                    })
                    .collect();
                format!("(array [{}])", a.join(" "))
            }
            Interpolated(parts) => {
                let a: Vec<_> = parts
                    .iter()
                    .map(|p| match p {
                        StringPart::Literal(_) => "lit".to_string(),
                        StringPart::Expr(e) => sexpr(src, e),
                    })
                    .collect();
                format!("(interp [{}])", a.join(" "))
            }
            Isset(xs) => format!("(isset {})", xs.len()),
            Empty(e) => format!("(empty {})", sexpr(src, e)),
            List(_) => "(list)".into(),
            Clone(e) => format!("(clone {})", sexpr(src, e)),
            Print(e) => format!("(print {})", sexpr(src, e)),
            Throw(e) => format!("(throw {})", sexpr(src, e)),
            Yield { .. } => "(yield)".into(),
            YieldFrom(e) => format!("(yield-from {})", sexpr(src, e)),
            VariableVariable(e) => format!("($ {})", sexpr(src, e)),
            NewAnon { args, .. } => format!("(new-anon [{}])", args.len()),
            Include { path, .. } => format!("(include {})", sexpr(src, path)),
            FirstClassCallable(e) => format!("(fcc {})", sexpr(src, e)),
            Error => "ERROR".into(),
        }
    }

    fn member(m: &MemberName) -> String {
        match m {
            MemberName::Identifier(s) => s.to_str_lossy().into_owned(),
            MemberName::Variable(s) => format!("${s}"),
            MemberName::Expr(_) => "{expr}".into(),
        }
    }

    fn binop(op: BinaryOp) -> &'static str {
        use BinaryOp::*;
        match op {
            Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%", Pow => "**",
            Concat => ".", Eq => "==", NotEq => "!=", Identical => "===",
            NotIdentical => "!==", Spaceship => "<=>", Lt => "<", Le => "<=", Gt => ">",
            Ge => ">=", BitAnd => "&", BitOr => "|", BitXor => "^", Shl => "<<", Shr => ">>",
            BoolAnd => "&&", BoolOr => "||", Coalesce => "??", LogicalAnd => "and",
            LogicalOr => "or", LogicalXor => "xor",
        }
    }

    fn unop(op: UnaryOp) -> &'static str {
        use UnaryOp::*;
        match op {
            Not => "!", Neg => "-", Pos => "+", BitNot => "~", Suppress => "@",
            Reference => "&",
            PreInc => "++", PreDec => "--", PostInc => "post++", PostDec => "post--",
        }
    }

    fn assert_sexpr(src: &str, expected: &str) {
        let (full, e) = parse_e(src);
        assert_eq!(sexpr(&full, &e), expected, "for input {src:?}");
    }

    #[test]
    fn arithmetic_precedence() {
        assert_sexpr("1 + 2 * 3", "(+ 1 (* 2 3))");
        assert_sexpr("1 * 2 + 3", "(+ (* 1 2) 3)");
        assert_sexpr("1 - 2 - 3", "(- (- 1 2) 3)"); // left-assoc
    }

    #[test]
    fn pow_is_right_associative_and_above_unary() {
        assert_sexpr("2 ** 3 ** 2", "(** 2 (** 3 2))");
        assert_sexpr("-2 ** 2", "(- (** 2 2))"); // -(2**2)
    }

    #[test]
    fn concat_binds_looser_than_add_php8() {
        // The 8.0 precedence change: `.` is looser than `+`/`-`.
        assert_sexpr("1 . 2 + 3", "(. 1 (+ 2 3))");
    }

    #[test]
    fn assignment_is_right_associative() {
        assert_sexpr("$a = $b = 1", "(= $a (= $b 1))");
        assert_sexpr("$a += 1", "(+ $a 1)");
    }

    #[test]
    fn coalesce_binds_tighter_than_ternary() {
        assert_sexpr("$a ?? $b ? $c : $d", "(?: (?? $a $b) $c $d)");
    }

    #[test]
    fn bang_sits_between_mul_and_instanceof() {
        assert_sexpr("!$a * $b", "(* (! $a) $b)");
        assert_sexpr("!$a instanceof B", "(! (instanceof $a B))");
    }

    #[test]
    fn postfix_chain_binds_tightest() {
        assert_sexpr("$a->b()->c[0]", "([] (-> (call (-> $a b) []) c) 0)");
        assert_sexpr("-$a->b", "(- (-> $a b))");
        assert_sexpr("Foo::BAR", "(:: Foo BAR)");
    }

    #[test]
    fn arrow_fn_and_match_and_array() {
        assert_sexpr("fn($x) => $x + 1", "(fn => (+ $x 1))");
        assert_sexpr("[1, 'k' => $v]", "(array [1 'k'=>$v])");
        assert_sexpr("f(1, $y)", "(call f [1 $y])");
        assert_sexpr("match($x) { 1 => 2 }", "(match $x)");
    }

    #[test]
    fn new_args_are_not_a_call() {
        assert_sexpr("new Foo($a)", "(new Foo [$a])");
        assert_sexpr("new $cls", "(new $cls [])");
    }

    #[test]
    fn interpolation_surfaces_variables() {
        // "a " · $b · " " · {$c->d}  — the space before `{` is its own literal chunk.
        assert_sexpr("\"a $b {$c->d}\"", "(interp [lit $b lit (-> $c d)])");
    }

    // --- program-level totality ------------------------------------------

    fn count_bad(stmts: &[Stmt], errors: &[ParseError]) -> (usize, usize) {
        fn walk_expr(e: &Expr, n: &mut usize) {
            if matches!(e.kind, ExprKind::Error) {
                *n += 1;
            }
        }
        let mut errs = errors.len();
        let mut unknowns = 0;
        fn walk(stmts: &[Stmt], unknowns: &mut usize, errs: &mut usize) {
            for s in stmts {
                match &s.kind {
                    StmtKind::Unknown(_) => *unknowns += 1,
                    StmtKind::Expr(e) | StmtKind::Return(Some(e)) => {
                        let mut n = 0;
                        walk_expr(e, &mut n);
                        *errs += n;
                    }
                    StmtKind::Echo(es) => {
                        for e in es {
                            let mut n = 0;
                            walk_expr(e, &mut n);
                            *errs += n;
                        }
                    }
                    StmtKind::Block(b) => walk(b, unknowns, errs),
                    StmtKind::If { then, else_ifs, else_, .. } => {
                        walk(then, unknowns, errs);
                        for (_, b) in else_ifs {
                            walk(b, unknowns, errs);
                        }
                        if let Some(b) = else_ {
                            walk(b, unknowns, errs);
                        }
                    }
                    StmtKind::While { body, .. } => walk(body, unknowns, errs),
                    _ => {}
                }
            }
        }
        walk(stmts, &mut unknowns, &mut errs);
        (unknowns, errs)
    }

    #[test]
    fn parses_realistic_program_without_errors() {
        let src = r#"<?php
$total = 0;
$items = [1, 2, 3];
$sum = fn($carry, $x) => $carry + $x;
echo "Total: {$total}", $extra;
if ($a && $b || !$c) {
    return $obj?->method($x, named: $y) ?? $default;
}
while ($i < 10) {
    $i = $i + 1;
}
"#;
        let (program, errors) = parse(src);
        let (_unknowns, errs) = count_bad(&program.stmts, &errors);
        assert_eq!(errs, 0, "expected no error nodes; errors={errors:?}");
    }

    #[test]
    fn statement_spans_are_ordered_and_in_bounds() {
        let src = "<?php $a = 1; echo $a; if ($a) { $b = 2; }";
        let (program, _errors) = parse(src);
        let mut prev_end = 0;
        for s in &program.stmts {
            assert!(s.span.start >= prev_end, "overlap at {:?}", s.kind);
            assert!(s.span.end <= src.len());
            prev_end = s.span.end;
        }
    }

    #[test]
    fn parses_class_declaration_and_following_code() {
        let src = "<?php class C extends B implements I { public int $x = 1; public function m(): void {} } $after = 1;";
        let (program, errors) = parse(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        let class = program.stmts.iter().find_map(|s| match &s.kind {
            StmtKind::ClassLike(c) => Some(c),
            _ => None,
        });
        let class = class.expect("class parsed");
        assert!(class.name.as_ref().is_some_and(|n| *n == "C"));
        assert!(class.extends.len() == 1 && class.extends[0] == "B");
        assert!(class.implements.len() == 1 && class.implements[0] == "I");
        assert_eq!(class.members.len(), 2); // property + method
        // The class does not swallow the following statement.
        assert!(program.stmts.iter().any(|s| matches!(&s.kind, StmtKind::Expr(_))));
    }

    #[test]
    fn parses_declarations_structurally() {
        // A grab-bag of declaration forms all parse without errors or Unknowns.
        let src = r#"<?php
namespace App;
use Foo\Bar as Baz;
use function strlen;
interface Shape { public function area(): float; }
trait T { public function hi() {} }
enum Suit: string {
    case Hearts = 'H';
    public function color(): string { return match($this) { default => 'red' }; }
}
abstract class Base implements Shape {
    public const array SIZES = [1, 2];
    public function __construct(private readonly int $id, public string $name = 'x') {}
    abstract public function area(): float;
}
function top(int ...$xs): int { return array_sum($xs); }
"#;
        let (_program, errors, unknowns) = super::parse_stats(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(unknowns, 0);
    }

    #[test]
    fn parses_property_hooks_and_asymmetric_visibility() {
        // PHP 8.4 features.
        let src = r#"<?php
class C {
    public int $x { get => $this->x * 2; set { $this->x = $value; } }
    public private(set) string $name = 'a';
}
"#;
        let (_program, errors, unknowns) = super::parse_stats(src);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(unknowns, 0);
    }
}

#[cfg(test)]
mod stress {
    use super::parse_stats;

    #[test]
    fn parses_a_dense_real_world_file_cleanly() {
        // A single file exercising most of the grammar at once: namespaces, use
        // (incl. `use function`), attributes, closures/arrow-fns, match, heredoc
        // with interpolation, null-safe chains, named args, spread, first-class
        // callables, try/catch/foreach, class/enum/trait with promotion, hooks,
        // and asymmetric visibility. Everything is valid PHP 8.4 → zero errors,
        // zero Unknown recovery.
        let src = r#"<?php
declare(strict_types=1);

namespace App\Services;

use App\Contracts\Handler;
use function strlen;

#[Route('/x')]
class Widget extends Base implements Handler {
    use LoggerTrait;

    public const array SIZES = [1, 2, 3];

    public int $count { get => $this->count; set { $this->count = $value; } }
    public private(set) string $name = 'w';

    public function __construct(
        public readonly int $id,
        private ?Handler $next = null,
        string ...$tags,
    ) {}

    public function handle(mixed $input): int|false {
        $config = ['debug' => true, 'level' => 3, ...$this->defaults];
        $make = fn ($x) => $x * 2;
        $fcc = strlen(...);
        $label = match ($input) {
            1, 2 => "low $input",
            default => <<<EOT
                high {$config['level']}
                EOT,
        };
        $r = $this->next?->handle($input, mode: $config['level']) ?? -1;
        foreach ($config as $k => &$v) {
            $v = $$k ?? 0;
        }
        try {
            return (int) ($r + strlen($label));
        } catch (\TypeError | \ValueError $e) {
            throw $e;
        } finally {
            unset($config);
        }
    }
}

enum Suit: string {
    case Hearts = 'H';
    public function color(): string {
        return match ($this) {
            Suit::Hearts => 'red',
            default => 'black',
        };
    }
}

$anon = new readonly class(1) implements Handler {
    public function __construct(public int $n) {}
};
"#;
        let (program, errors, unknowns) = parse_stats(src);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(unknowns, 0, "unexpected Unknown recovery nodes");
        for s in &program.stmts {
            assert!(s.span.end <= src.len(), "span out of bounds: {:?}", s.kind);
        }
    }
}

use crate::span::Span;

/// PHP 8.4 keywords, as classified by the lexer.
///
/// Note: `int`, `float`, `string`, `bool`, `void`, `self`, `parent`, `true`,
/// `false`, `null`, `mixed`, `never`, `iterable`, `object` are **not** keywords
/// in PHP's tokenizer — they are plain identifiers (`T_STRING`) resolved by the
/// parser/runtime. We match that so downstream code and the `token_get_all`
/// oracle agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyword {
    Abstract, And, Array, As, Break, Callable, Case, Catch, Class, Clone, Const,
    Continue, Declare, Default, Do, Echo, Else, ElseIf, Empty, EndDeclare, EndFor,
    EndForeach, EndIf, EndSwitch, EndWhile, Enum, Extends, Final, Finally, Fn, For,
    Foreach, Function, Global, Goto, If, Implements, Include, IncludeOnce, InstanceOf,
    InsteadOf, Interface, Isset, List, Match, Namespace, New, Or, Print, Private,
    Protected, Public, Readonly, Require, RequireOnce, Return, Static, Switch, Throw,
    Trait, Try, Unset, Use, Var, While, Xor, Yield, YieldFrom,
}

impl Keyword {
    /// Case-insensitive lookup (PHP keywords are case-insensitive).
    pub fn from_ident(text: &[u8]) -> Option<Keyword> {
        // Fast reject: keywords are ASCII; anything with a non-ASCII byte can't be one.
        if !text.is_ascii() {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        Some(match lower.as_slice() {
            b"abstract" => Keyword::Abstract,
            b"and" => Keyword::And,
            b"array" => Keyword::Array,
            b"as" => Keyword::As,
            b"break" => Keyword::Break,
            b"callable" => Keyword::Callable,
            b"case" => Keyword::Case,
            b"catch" => Keyword::Catch,
            b"class" => Keyword::Class,
            b"clone" => Keyword::Clone,
            b"const" => Keyword::Const,
            b"continue" => Keyword::Continue,
            b"declare" => Keyword::Declare,
            b"default" => Keyword::Default,
            b"do" => Keyword::Do,
            b"echo" => Keyword::Echo,
            b"else" => Keyword::Else,
            b"elseif" => Keyword::ElseIf,
            b"empty" => Keyword::Empty,
            b"enddeclare" => Keyword::EndDeclare,
            b"endfor" => Keyword::EndFor,
            b"endforeach" => Keyword::EndForeach,
            b"endif" => Keyword::EndIf,
            b"endswitch" => Keyword::EndSwitch,
            b"endwhile" => Keyword::EndWhile,
            b"enum" => Keyword::Enum,
            b"extends" => Keyword::Extends,
            b"final" => Keyword::Final,
            b"finally" => Keyword::Finally,
            b"fn" => Keyword::Fn,
            b"for" => Keyword::For,
            b"foreach" => Keyword::Foreach,
            b"function" => Keyword::Function,
            b"global" => Keyword::Global,
            b"goto" => Keyword::Goto,
            b"if" => Keyword::If,
            b"implements" => Keyword::Implements,
            b"include" => Keyword::Include,
            b"include_once" => Keyword::IncludeOnce,
            b"instanceof" => Keyword::InstanceOf,
            b"insteadof" => Keyword::InsteadOf,
            b"interface" => Keyword::Interface,
            b"isset" => Keyword::Isset,
            b"list" => Keyword::List,
            b"match" => Keyword::Match,
            b"namespace" => Keyword::Namespace,
            b"new" => Keyword::New,
            b"or" => Keyword::Or,
            b"print" => Keyword::Print,
            b"private" => Keyword::Private,
            b"protected" => Keyword::Protected,
            b"public" => Keyword::Public,
            b"readonly" => Keyword::Readonly,
            b"require" => Keyword::Require,
            b"require_once" => Keyword::RequireOnce,
            b"return" => Keyword::Return,
            b"static" => Keyword::Static,
            b"switch" => Keyword::Switch,
            b"throw" => Keyword::Throw,
            b"trait" => Keyword::Trait,
            b"try" => Keyword::Try,
            b"unset" => Keyword::Unset,
            b"use" => Keyword::Use,
            b"var" => Keyword::Var,
            b"while" => Keyword::While,
            b"xor" => Keyword::Xor,
            b"yield" => Keyword::Yield,
            _ => return None,
        })
    }
}

/// PHP type-cast operators, e.g. `(int)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cast {
    Int,
    Bool,
    Float,
    String,
    Array,
    Object,
    Unset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Eof,

    // Tags & inline HTML.
    InlineHtml,
    OpenTag,     // `<?php` (+ one trailing whitespace char, PHP-style)
    OpenTagEcho, // `<?=`
    CloseTag,    // `?>` (+ one trailing newline, PHP-style)

    // Trivia.
    Whitespace,
    Comment,    // `//`, `#`, `/* */`
    DocComment, // `/** */`

    // Names & literals.
    Identifier, // T_STRING
    Keyword(Keyword),
    Variable,        // `$name`
    Int,             // T_LNUMBER
    Float,           // T_DNUMBER
    ConstantString,  // whole single-quoted, or double-quoted with no interpolation
    MagicConstant,   // `__LINE__`, `__FILE__`, `__PROPERTY__`, ...

    // String interpolation machinery (see the module-level docs on the lexer).
    DoubleQuote,     // `"` delimiter (open or close) of an interpolated string
    Backtick,        // `` ` `` delimiter of a shell-exec string
    EncapsedText,    // T_ENCAPSED_AND_WHITESPACE — a literal chunk inside a string
    NumString,       // T_NUM_STRING — an integer offset inside `"$a[0]"`
    StartHeredoc,    // `<<<LABEL\n` or `<<<'LABEL'\n`
    EndHeredoc,      // closing `LABEL` (with any leading indentation)
    CurlyOpen,       // `{` of a `{$...}` complex interpolation
    DollarOpenCurly, // `${`
    StringVarname,   // the name inside `${ name }`

    Cast(Cast),
    Attribute, // `#[`

    // Punctuation.
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Comma,
    Semicolon,
    Colon,
    DoubleColon, // `::`
    DoubleArrow, // `=>`
    Arrow,       // `->`
    NullsafeArrow, // `?->`
    Question,
    Coalesce, // `??`
    Ellipsis, // `...`
    Backslash,
    At,
    Dollar, // a lone `$`

    // Operators.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pow, // `**`
    Dot,
    Assign, // `=`
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    PowAssign,
    DotAssign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    ShlAssign,
    ShrAssign,
    CoalesceAssign, // `??=`
    Eq,             // `==`
    NotEq,          // `!=` or `<>`
    Identical,      // `===`
    NotIdentical,   // `!==`
    Lt,
    Gt,
    Le,
    Ge,
    Spaceship, // `<=>`
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl, // `<<`
    Shr, // `>>`
    BoolAnd, // `&&`
    BoolOr,  // `||`
    Bang,
    Inc, // `++`
    Dec, // `--`

    /// A byte we couldn't classify. Should never appear for valid PHP; used so
    /// the lexer is total (never panics) and the round-trip invariant holds.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl TokenKind {
    /// Trivia is skipped when walking the stream structurally.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::Comment | TokenKind::DocComment
        )
    }
}

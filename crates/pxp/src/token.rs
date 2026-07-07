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
    pub fn from_ident(text: &str) -> Option<Keyword> {
        // Fast reject: keywords are ASCII; anything with a non-ASCII byte can't be one.
        if !text.is_ascii() {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        Some(match lower.as_str() {
            "abstract" => Keyword::Abstract,
            "and" => Keyword::And,
            "array" => Keyword::Array,
            "as" => Keyword::As,
            "break" => Keyword::Break,
            "callable" => Keyword::Callable,
            "case" => Keyword::Case,
            "catch" => Keyword::Catch,
            "class" => Keyword::Class,
            "clone" => Keyword::Clone,
            "const" => Keyword::Const,
            "continue" => Keyword::Continue,
            "declare" => Keyword::Declare,
            "default" => Keyword::Default,
            "do" => Keyword::Do,
            "echo" => Keyword::Echo,
            "else" => Keyword::Else,
            "elseif" => Keyword::ElseIf,
            "empty" => Keyword::Empty,
            "enddeclare" => Keyword::EndDeclare,
            "endfor" => Keyword::EndFor,
            "endforeach" => Keyword::EndForeach,
            "endif" => Keyword::EndIf,
            "endswitch" => Keyword::EndSwitch,
            "endwhile" => Keyword::EndWhile,
            "enum" => Keyword::Enum,
            "extends" => Keyword::Extends,
            "final" => Keyword::Final,
            "finally" => Keyword::Finally,
            "fn" => Keyword::Fn,
            "for" => Keyword::For,
            "foreach" => Keyword::Foreach,
            "function" => Keyword::Function,
            "global" => Keyword::Global,
            "goto" => Keyword::Goto,
            "if" => Keyword::If,
            "implements" => Keyword::Implements,
            "include" => Keyword::Include,
            "include_once" => Keyword::IncludeOnce,
            "instanceof" => Keyword::InstanceOf,
            "insteadof" => Keyword::InsteadOf,
            "interface" => Keyword::Interface,
            "isset" => Keyword::Isset,
            "list" => Keyword::List,
            "match" => Keyword::Match,
            "namespace" => Keyword::Namespace,
            "new" => Keyword::New,
            "or" => Keyword::Or,
            "print" => Keyword::Print,
            "private" => Keyword::Private,
            "protected" => Keyword::Protected,
            "public" => Keyword::Public,
            "readonly" => Keyword::Readonly,
            "require" => Keyword::Require,
            "require_once" => Keyword::RequireOnce,
            "return" => Keyword::Return,
            "static" => Keyword::Static,
            "switch" => Keyword::Switch,
            "throw" => Keyword::Throw,
            "trait" => Keyword::Trait,
            "try" => Keyword::Try,
            "unset" => Keyword::Unset,
            "use" => Keyword::Use,
            "var" => Keyword::Var,
            "while" => Keyword::While,
            "xor" => Keyword::Xor,
            "yield" => Keyword::Yield,
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

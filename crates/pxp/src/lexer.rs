//! A hand-written, stateful PHP 8.4 lexer.
//!
//! PHP's tokenizer is a mode machine, and this mirrors it with a small state
//! stack. The base state is [`State::Html`]; an open tag pushes [`State::Scripting`];
//! a close tag pops back. Interpolated strings push a string state whose body is
//! scanned specially, and a `{$…}` complex interpolation pushes a *nested*
//! scripting frame whose matching `}` pops back into the string. That nesting is
//! why a stack (not a single enum) is needed: `"{$a["{$b}"]}"` is legal PHP.
//!
//! Design choices for this transpiler:
//! * **Lossless.** Whitespace and comments are real tokens; token spans tile the
//!   whole file, so the source can be reconstructed byte-for-byte (asserted in
//!   tests). Untouched code is copied from the source buffer during emit.
//! * **Boundaries match `token_get_all`** for scripting-mode tokens (open-tag
//!   whitespace folding, close-tag newline, casts, operators), so PHP's own
//!   tokenizer can be used as a differential oracle.
//! * **Simple interpolation emits `Variable` tokens.** `"$a"` and `"{$a->b}"`
//!   surface `$a` to downstream passes — this is what lets capture analysis see
//!   variables used inside strings.
//!
//! Known simplifications (documented, refined later): inside a simple
//! interpolation, `$a[0]` / `$a->b` offsets and property names are folded into
//! the surrounding encapsed text rather than split into `T_NUM_STRING` /
//! `T_STRING` tokens (variables inside, like `"$a[$i]"`, are still surfaced);
//! `${name}` yields `StringVarname`, not a `Variable`; `__halt_compiler` does not
//! stop the lexer.

use crate::span::Span;
use crate::token::{Cast, Keyword, Token, TokenKind};

#[derive(Clone, Debug)]
enum State {
    Html,
    /// `interp` is `Some(depth)` when this frame is inside a `{$…}`/`${…}`
    /// interpolation; the `}` that returns depth to 0 pops back to the string.
    Scripting { interp: Option<u32> },
    DoubleQuote,
    Backtick,
    Heredoc { label: Vec<u8> },
    Nowdoc { label: Vec<u8> },
    LookingForVarname,
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    states: Vec<State>,
    /// Set right after `->`/`?->`: the next identifier is a property name and is
    /// forced to `Identifier` even if it spells a keyword (`$o->class`).
    expect_property: bool,
}

pub fn lex(source: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        src: source.as_bytes(),
        pos: 0,
        states: vec![State::Html],
        expect_property: false,
    };
    let mut out = Vec::new();
    while lexer.pos < lexer.src.len() {
        let before = lexer.pos;
        let tok = lexer.next_token();
        debug_assert!(lexer.pos > before, "lexer made no progress at {before}");

        // Maintain the property-access flag across trivia only.
        match tok.kind {
            TokenKind::Arrow | TokenKind::NullsafeArrow => lexer.expect_property = true,
            k if !k.is_trivia() => lexer.expect_property = false,
            _ => {}
        }
        out.push(tok);
    }
    out.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(lexer.src.len(), lexer.src.len()),
    });
    out
}

impl<'a> Lexer<'a> {
    fn next_token(&mut self) -> Token {
        match self.states.last().expect("state stack never empties") {
            State::Html => self.lex_html(),
            State::Scripting { .. } => self.lex_scripting(),
            State::DoubleQuote => self.lex_string_body(StringFlavor::Double),
            State::Backtick => self.lex_string_body(StringFlavor::Backtick),
            State::Heredoc { .. } => self.lex_string_body(StringFlavor::Heredoc),
            State::Nowdoc { .. } => self.lex_nowdoc(),
            State::LookingForVarname => self.lex_looking_for_varname(),
        }
    }

    // --- byte helpers -----------------------------------------------------

    fn at(&self, offset: usize) -> u8 {
        self.src.get(self.pos + offset).copied().unwrap_or(0)
    }

    fn starts(&self, needle: &[u8]) -> bool {
        self.src[self.pos..].starts_with(needle)
    }

    fn tok(&self, kind: TokenKind, start: usize) -> Token {
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    fn at_line_start(&self, pos: usize) -> bool {
        pos == 0 || self.src[pos - 1] == b'\n'
    }

    // --- HTML mode --------------------------------------------------------

    fn lex_html(&mut self) -> Token {
        let start = self.pos;
        if self.starts(b"<?php") {
            self.pos += 5;
            // T_OPEN_TAG folds one trailing whitespace char (a space/tab or a
            // full newline), matching token_get_all.
            match self.at(0) {
                b' ' | b'\t' => self.pos += 1,
                b'\n' => self.pos += 1,
                b'\r' => self.pos += if self.at(1) == b'\n' { 2 } else { 1 },
                _ => {}
            }
            self.states.push(State::Scripting { interp: None });
            return self.tok(TokenKind::OpenTag, start);
        }
        if self.starts(b"<?=") {
            self.pos += 3;
            self.states.push(State::Scripting { interp: None });
            return self.tok(TokenKind::OpenTagEcho, start);
        }
        if self.starts(b"<?") {
            self.pos += 2;
            self.states.push(State::Scripting { interp: None });
            return self.tok(TokenKind::OpenTag, start);
        }
        // Inline HTML up to the next PHP open.
        while self.pos < self.src.len() && !self.starts(b"<?") {
            self.pos += 1;
        }
        self.tok(TokenKind::InlineHtml, start)
    }

    // --- scripting mode ---------------------------------------------------

    fn lex_scripting(&mut self) -> Token {
        let start = self.pos;
        let c = self.src[self.pos];

        if matches!(c, b' ' | b'\t' | b'\r' | b'\n') {
            while self.pos < self.src.len() && matches!(self.src[self.pos], b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            }
            return self.tok(TokenKind::Whitespace, start);
        }

        // Close tag — only at top level (not inside a `{$…}` interpolation).
        if self.starts(b"?>") && self.current_interp().is_none() {
            self.pos += 2;
            match self.at(0) {
                b'\n' => self.pos += 1,
                b'\r' => self.pos += if self.at(1) == b'\n' { 2 } else { 1 },
                _ => {}
            }
            self.states.pop(); // back to Html
            return self.tok(TokenKind::CloseTag, start);
        }

        // Attributes and comments.
        if self.starts(b"#[") {
            self.pos += 2;
            return self.tok(TokenKind::Attribute, start);
        }
        if self.starts(b"//") || c == b'#' {
            while self.pos < self.src.len() && self.src[self.pos] != b'\n' && !self.starts(b"?>") {
                self.pos += 1;
            }
            return self.tok(TokenKind::Comment, start);
        }
        if self.starts(b"/*") {
            let doc = self.starts(b"/**") && !self.starts(b"/**/");
            self.pos += 2;
            while self.pos < self.src.len() && !self.starts(b"*/") {
                self.pos += 1;
            }
            if self.starts(b"*/") {
                self.pos += 2;
            }
            let kind = if doc { TokenKind::DocComment } else { TokenKind::Comment };
            return self.tok(kind, start);
        }

        // Heredoc / nowdoc opener.
        if self.starts(b"<<<") {
            return self.lex_heredoc_start(start);
        }

        // Strings.
        if c == b'\'' {
            self.consume_single_quoted();
            return self.tok(TokenKind::ConstantString, start);
        }
        if c == b'"' {
            return self.lex_double_quote_open(start);
        }
        if c == b'`' {
            self.pos += 1;
            self.states.push(State::Backtick);
            return self.tok(TokenKind::Backtick, start);
        }

        // Casts: `(int)`, `( array )`, ...
        if c == b'(' {
            if let Some(tok) = self.try_cast(start) {
                return tok;
            }
        }

        // Numbers.
        if c.is_ascii_digit() || (c == b'.' && self.at(1).is_ascii_digit()) {
            return self.lex_number(start);
        }

        // Variables.
        if c == b'$' && is_ident_start(self.at(1)) {
            self.pos += 1;
            while self.pos < self.src.len() && is_ident_cont(self.src[self.pos]) {
                self.pos += 1;
            }
            return self.tok(TokenKind::Variable, start);
        }

        // Identifiers / keywords / magic constants.
        if is_ident_start(c) {
            return self.lex_identifier(start);
        }

        // Braces need interpolation-depth bookkeeping.
        if c == b'{' {
            self.pos += 1;
            if let Some(State::Scripting { interp: Some(d) }) = self.states.last_mut() {
                *d += 1;
            }
            return self.tok(TokenKind::LeftBrace, start);
        }
        if c == b'}' {
            let closes_interp = matches!(self.current_interp(), Some(0));
            self.pos += 1;
            if closes_interp {
                self.states.pop();
            } else if let Some(State::Scripting { interp: Some(d) }) = self.states.last_mut() {
                *d -= 1;
            }
            return self.tok(TokenKind::RightBrace, start);
        }

        self.lex_operator(start)
    }

    fn current_interp(&self) -> Option<u32> {
        match self.states.last() {
            Some(State::Scripting { interp }) => *interp,
            _ => None,
        }
    }

    fn lex_identifier(&mut self, start: usize) -> Token {
        while self.pos < self.src.len() && is_ident_cont(self.src[self.pos]) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");

        if self.expect_property {
            return self.tok(TokenKind::Identifier, start);
        }
        if is_magic_constant(text) {
            return self.tok(TokenKind::MagicConstant, start);
        }
        if let Some(kw) = Keyword::from_ident(text) {
            if kw == Keyword::Yield {
                if let Some(end) = self.yield_from_end() {
                    self.pos = end;
                    return self.tok(TokenKind::Keyword(Keyword::YieldFrom), start);
                }
            }
            return self.tok(TokenKind::Keyword(kw), start);
        }
        self.tok(TokenKind::Identifier, start)
    }

    /// If positioned right after `yield`, and `\s+from` (word) follows, return
    /// the end offset of `from` so `yield from` becomes one token.
    fn yield_from_end(&self) -> Option<usize> {
        let mut i = self.pos;
        if i >= self.src.len() || !matches!(self.src[i], b' ' | b'\t' | b'\r' | b'\n') {
            return None;
        }
        while i < self.src.len() && matches!(self.src[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if self.src[i..].len() >= 4
            && self.src[i..i + 4].eq_ignore_ascii_case(b"from")
            && !is_ident_cont(self.src.get(i + 4).copied().unwrap_or(0))
        {
            Some(i + 4)
        } else {
            None
        }
    }

    fn lex_number(&mut self, start: usize) -> Token {
        let mut is_float = self.src[self.pos] == b'.';
        if self.starts(b"0x") || self.starts(b"0X") {
            self.pos += 2;
            while self.pos < self.src.len() && (self.src[self.pos].is_ascii_hexdigit() || self.src[self.pos] == b'_') {
                self.pos += 1;
            }
            return self.tok(TokenKind::Int, start);
        }
        if self.starts(b"0b") || self.starts(b"0B") || self.starts(b"0o") || self.starts(b"0O") {
            self.pos += 2;
            while self.pos < self.src.len() && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_') {
                self.pos += 1;
            }
            return self.tok(TokenKind::Int, start);
        }
        // Decimal / float.
        while self.pos < self.src.len() && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'_') {
            self.pos += 1;
        }
        if self.src.get(self.pos) == Some(&b'.') && self.at(1) != b'.' {
            is_float = true;
            self.pos += 1;
            while self.pos < self.src.len() && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'_') {
                self.pos += 1;
            }
        }
        if matches!(self.src.get(self.pos), Some(b'e') | Some(b'E')) {
            let mut j = self.pos + 1;
            if matches!(self.src.get(j), Some(b'+') | Some(b'-')) {
                j += 1;
            }
            if matches!(self.src.get(j), Some(d) if d.is_ascii_digit()) {
                is_float = true;
                self.pos = j;
                while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            }
        }
        let kind = if is_float { TokenKind::Float } else { TokenKind::Int };
        self.tok(kind, start)
    }

    fn try_cast(&mut self, start: usize) -> Option<Token> {
        let mut i = self.pos + 1; // past '('
        while matches!(self.src.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        let word_start = i;
        while matches!(self.src.get(i), Some(b) if b.is_ascii_alphabetic()) {
            i += 1;
        }
        let word = std::str::from_utf8(&self.src[word_start..i]).ok()?.to_ascii_lowercase();
        let cast = match word.as_str() {
            "int" | "integer" => Cast::Int,
            "bool" | "boolean" => Cast::Bool,
            "float" | "double" | "real" => Cast::Float,
            "string" | "binary" => Cast::String,
            "array" => Cast::Array,
            "object" => Cast::Object,
            "unset" => Cast::Unset,
            _ => return None,
        };
        while matches!(self.src.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        if self.src.get(i) == Some(&b')') {
            self.pos = i + 1;
            Some(self.tok(TokenKind::Cast(cast), start))
        } else {
            None
        }
    }

    fn lex_operator(&mut self, start: usize) -> Token {
        use TokenKind::*;
        // Longest match first.
        let (len, kind) = if self.starts(b"<=>") {
            (3, Spaceship)
        } else if self.starts(b"===") {
            (3, Identical)
        } else if self.starts(b"!==") {
            (3, NotIdentical)
        } else if self.starts(b"**=") {
            (3, PowAssign)
        } else if self.starts(b"<<=") {
            (3, ShlAssign)
        } else if self.starts(b">>=") {
            (3, ShrAssign)
        } else if self.starts(b"??=") {
            (3, CoalesceAssign)
        } else if self.starts(b"...") {
            (3, Ellipsis)
        } else if self.starts(b"?->") {
            (3, NullsafeArrow)
        } else if self.starts(b"==") {
            (2, Eq)
        } else if self.starts(b"!=") || self.starts(b"<>") {
            (2, NotEq)
        } else if self.starts(b"<=") {
            (2, Le)
        } else if self.starts(b">=") {
            (2, Ge)
        } else if self.starts(b"&&") {
            (2, BoolAnd)
        } else if self.starts(b"||") {
            (2, BoolOr)
        } else if self.starts(b"++") {
            (2, Inc)
        } else if self.starts(b"--") {
            (2, Dec)
        } else if self.starts(b"->") {
            (2, Arrow)
        } else if self.starts(b"=>") {
            (2, DoubleArrow)
        } else if self.starts(b"::") {
            (2, DoubleColon)
        } else if self.starts(b"**") {
            (2, Pow)
        } else if self.starts(b"<<") {
            (2, Shl)
        } else if self.starts(b">>") {
            (2, Shr)
        } else if self.starts(b"??") {
            (2, Coalesce)
        } else if self.starts(b"+=") {
            (2, PlusAssign)
        } else if self.starts(b"-=") {
            (2, MinusAssign)
        } else if self.starts(b"*=") {
            (2, StarAssign)
        } else if self.starts(b"/=") {
            (2, SlashAssign)
        } else if self.starts(b"%=") {
            (2, PercentAssign)
        } else if self.starts(b".=") {
            (2, DotAssign)
        } else if self.starts(b"&=") {
            (2, AmpAssign)
        } else if self.starts(b"|=") {
            (2, PipeAssign)
        } else if self.starts(b"^=") {
            (2, CaretAssign)
        } else {
            let single = match self.src[self.pos] {
                b'+' => Plus,
                b'-' => Minus,
                b'*' => Star,
                b'/' => Slash,
                b'%' => Percent,
                b'.' => Dot,
                b'=' => Assign,
                b'<' => Lt,
                b'>' => Gt,
                b'&' => Amp,
                b'|' => Pipe,
                b'^' => Caret,
                b'~' => Tilde,
                b'!' => Bang,
                b'?' => Question,
                b':' => Colon,
                b'(' => LeftParen,
                b')' => RightParen,
                b'[' => LeftBracket,
                b']' => RightBracket,
                b',' => Comma,
                b';' => Semicolon,
                b'@' => At,
                b'\\' => Backslash,
                b'$' => Dollar,
                _ => Unknown,
            };
            (1, single)
        };
        self.pos += len;
        self.tok(kind, start)
    }

    // --- strings ----------------------------------------------------------

    fn consume_single_quoted(&mut self) {
        self.pos += 1; // opening '
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'\\' => self.pos += 1 + utf8_len(self.at(1)),
                b'\'' => {
                    self.pos += 1;
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    /// A double-quoted string with no interpolation is one `ConstantString`;
    /// otherwise emit the opening `"` and switch to body scanning.
    fn lex_double_quote_open(&mut self, start: usize) -> Token {
        let mut i = self.pos + 1;
        let mut interpolated = false;
        while i < self.src.len() {
            match self.src[i] {
                b'\\' => i += 1 + utf8_len(self.src.get(i + 1).copied().unwrap_or(0)),
                b'"' => break,
                b'$' if is_ident_start(self.src.get(i + 1).copied().unwrap_or(0))
                    || self.src.get(i + 1) == Some(&b'{') =>
                {
                    interpolated = true;
                    break;
                }
                b'{' if self.src.get(i + 1) == Some(&b'$') => {
                    interpolated = true;
                    break;
                }
                _ => i += 1,
            }
        }
        if interpolated {
            self.pos += 1;
            self.states.push(State::DoubleQuote);
            self.tok(TokenKind::DoubleQuote, start)
        } else {
            self.pos = (i + 1).min(self.src.len()); // include closing quote if present
            self.tok(TokenKind::ConstantString, start)
        }
    }

    fn lex_heredoc_start(&mut self, start: usize) -> Token {
        self.pos += 3; // <<<
        while matches!(self.at(0), b' ' | b'\t') {
            self.pos += 1;
        }
        let quote = self.at(0);
        let nowdoc = quote == b'\'';
        if quote == b'\'' || quote == b'"' {
            self.pos += 1;
        }
        let label_start = self.pos;
        while self.pos < self.src.len() && is_ident_cont(self.src[self.pos]) {
            self.pos += 1;
        }
        let label = self.src[label_start..self.pos].to_vec();
        if (quote == b'\'' || quote == b'"') && self.at(0) == quote {
            self.pos += 1;
        }
        // Consume the rest of the opening line, including its newline.
        while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
            self.pos += 1;
        }
        if self.pos < self.src.len() {
            self.pos += 1; // the newline
        }
        self.states.push(if nowdoc {
            State::Nowdoc { label }
        } else {
            State::Heredoc { label }
        });
        self.tok(TokenKind::StartHeredoc, start)
    }

    fn current_label(&self) -> Vec<u8> {
        match self.states.last() {
            Some(State::Heredoc { label }) | Some(State::Nowdoc { label }) => label.clone(),
            _ => Vec::new(),
        }
    }

    /// If `line_start` begins the heredoc/nowdoc closing marker, return the byte
    /// offset just past the label (the `EndHeredoc` span is `line_start..end`).
    fn label_close_at(&self, line_start: usize, label: &[u8]) -> Option<usize> {
        let mut i = line_start;
        while matches!(self.src.get(i), Some(b' ') | Some(b'\t')) {
            i += 1;
        }
        if self.src[i..].starts_with(label) {
            let after = i + label.len();
            if !is_ident_cont(self.src.get(after).copied().unwrap_or(0)) {
                return Some(after);
            }
        }
        None
    }

    fn lex_string_body(&mut self, flavor: StringFlavor) -> Token {
        let start = self.pos;

        // Terminator.
        match flavor {
            StringFlavor::Double if self.src[self.pos] == b'"' => {
                self.pos += 1;
                self.states.pop();
                return self.tok(TokenKind::DoubleQuote, start);
            }
            StringFlavor::Backtick if self.src[self.pos] == b'`' => {
                self.pos += 1;
                self.states.pop();
                return self.tok(TokenKind::Backtick, start);
            }
            StringFlavor::Heredoc if self.at_line_start(self.pos) => {
                let label = self.current_label();
                if let Some(end) = self.label_close_at(self.pos, &label) {
                    self.pos = end;
                    self.states.pop();
                    return self.tok(TokenKind::EndHeredoc, start);
                }
            }
            _ => {}
        }

        // Interpolation triggers.
        if self.src[self.pos] == b'{' && self.at(1) == b'$' {
            self.pos += 1;
            self.states.push(State::Scripting { interp: Some(0) });
            return self.tok(TokenKind::CurlyOpen, start);
        }
        if self.src[self.pos] == b'$' && self.at(1) == b'{' {
            self.pos += 2;
            self.states.push(State::LookingForVarname);
            return self.tok(TokenKind::DollarOpenCurly, start);
        }
        if self.src[self.pos] == b'$' && is_ident_start(self.at(1)) {
            self.pos += 1;
            while self.pos < self.src.len() && is_ident_cont(self.src[self.pos]) {
                self.pos += 1;
            }
            return self.tok(TokenKind::Variable, start);
        }

        // Encapsed literal chunk up to the next trigger/terminator.
        let heredoc_label = if matches!(flavor, StringFlavor::Heredoc) {
            self.current_label()
        } else {
            Vec::new()
        };
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'\\' {
                self.pos += 1 + utf8_len(self.at(1));
                continue;
            }
            match flavor {
                StringFlavor::Double if c == b'"' => break,
                StringFlavor::Backtick if c == b'`' => break,
                StringFlavor::Heredoc if c == b'\n' => {
                    if self.label_close_at(self.pos + 1, &heredoc_label).is_some() {
                        self.pos += 1; // keep the newline with the body
                        break;
                    }
                }
                _ => {}
            }
            if c == b'$' && (is_ident_start(self.at(1)) || self.at(1) == b'{') {
                break;
            }
            if c == b'{' && self.at(1) == b'$' {
                break;
            }
            self.pos += 1;
        }
        self.tok(TokenKind::EncapsedText, start)
    }

    fn lex_nowdoc(&mut self) -> Token {
        let start = self.pos;
        let label = self.current_label();
        if self.at_line_start(self.pos) {
            if let Some(end) = self.label_close_at(self.pos, &label) {
                self.pos = end;
                self.states.pop();
                return self.tok(TokenKind::EndHeredoc, start);
            }
        }
        while self.pos < self.src.len() {
            if self.src[self.pos] == b'\n' && self.label_close_at(self.pos + 1, &label).is_some() {
                self.pos += 1;
                break;
            }
            self.pos += 1;
        }
        self.tok(TokenKind::EncapsedText, start)
    }

    fn lex_looking_for_varname(&mut self) -> Token {
        let start = self.pos;
        // `${ name }` / `${ name[` -> emit the varname, then continue in scripting.
        if is_ident_start(self.at(0)) {
            let mut i = self.pos;
            while i < self.src.len() && is_ident_cont(self.src[i]) {
                i += 1;
            }
            let after = self.src.get(i).copied().unwrap_or(0);
            if after == b'}' || after == b'[' {
                self.pos = i;
                *self.states.last_mut().unwrap() = State::Scripting { interp: Some(0) };
                return self.tok(TokenKind::StringVarname, start);
            }
        }
        // Otherwise it's an expression (`${ $x }`): become scripting and retry.
        *self.states.last_mut().unwrap() = State::Scripting { interp: Some(0) };
        self.lex_scripting()
    }
}

#[derive(Clone, Copy)]
enum StringFlavor {
    Double,
    Backtick,
    Heredoc,
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic() || b >= 0x80
}

fn is_ident_cont(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || b >= 0x80
}

fn is_magic_constant(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "__LINE__"
            | "__FILE__"
            | "__DIR__"
            | "__FUNCTION__"
            | "__CLASS__"
            | "__TRAIT__"
            | "__METHOD__"
            | "__NAMESPACE__"
            | "__PROPERTY__"
    )
}

/// Length in bytes of the UTF-8 sequence beginning with `lead`.
fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >= 0xF0 {
        4
    } else if lead >= 0xE0 {
        3
    } else if lead >= 0xC0 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::lex;
    use crate::token::{Cast, Keyword, TokenKind};
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).iter().map(|t| t.kind).filter(|k| *k != TokenKind::Eof).collect()
    }

    fn var_texts(src: &str) -> Vec<String> {
        lex(src)
            .iter()
            .filter(|t| t.kind == TokenKind::Variable)
            .map(|t| t.span.as_str(src).to_string())
            .collect()
    }

    /// The lossless invariant: token spans tile the whole file with no gaps or
    /// overlaps, so the source reconstructs byte-for-byte.
    fn assert_round_trip(src: &str) {
        let toks = lex(src);
        let mut cursor = 0;
        for t in &toks {
            assert_eq!(t.span.start, cursor, "gap/overlap before {:?} in {src:?}", t.kind);
            cursor = t.span.end;
        }
        assert_eq!(cursor, src.len(), "did not cover all bytes of {src:?}");
    }

    #[test]
    fn round_trips_diverse_sources() {
        for src in [
            "<?php\n$a = 1 + 2;\necho $a;\n",
            "<html><?php echo $x; ?></html>",
            "<?php $s = \"a $b {$c->d} e\"; $t = 'raw';",
            "<?php $h = <<<EOT\n  line $x\n  EOT;\n",
            "<?php $n = <<<'RAW'\n$not_interpolated\nRAW;\n",
            "<?php fn($x) => { return $x ** 2; };",
            "<?php #[Attr('/x')] class C { public int $n; }",
            "<?php $café = 1; // unicode ident\n",
            "<?php $o?->m() ?? (int)$y <=> $z;",
        ] {
            assert_round_trip(src);
        }
    }

    #[test]
    fn open_tag_folds_one_whitespace() {
        // `<?php ` is 6 bytes (folds one space), matching token_get_all.
        let toks = lex("<?php  \n$x;");
        assert_eq!(toks[0].kind, TokenKind::OpenTag);
        assert_eq!(toks[0].span.end, 6);
        assert_eq!(toks[1].kind, TokenKind::Whitespace); // " \n"
    }

    #[test]
    fn close_tag_eats_newline_then_html() {
        let ks = kinds("<?php $x; ?>\nHELLO<?php");
        assert!(ks.contains(&TokenKind::CloseTag));
        assert!(ks.contains(&TokenKind::InlineHtml));
    }

    #[test]
    fn simple_interpolation_surfaces_variables() {
        assert_eq!(var_texts("<?php \"a $b c $d e\";"), vec!["$b", "$d"]);
    }

    #[test]
    fn complex_interpolation_surfaces_variables() {
        assert_eq!(var_texts("<?php \"x {$a->b()} y\";"), vec!["$a"]);
    }

    #[test]
    fn non_interpolated_double_quote_is_one_token() {
        let ks = kinds("<?php \"just text\";");
        assert!(ks.contains(&TokenKind::ConstantString));
        assert!(!ks.contains(&TokenKind::DoubleQuote));
    }

    #[test]
    fn heredoc_and_nowdoc() {
        assert_eq!(var_texts("<?php <<<EOT\n  a $b c\n  EOT;\n"), vec!["$b"]);
        // Nowdoc does not interpolate.
        assert!(var_texts("<?php <<<'EOT'\nraw $b\nEOT;\n").is_empty());
    }

    #[test]
    fn numbers() {
        assert_eq!(kinds("<?php 0x1F")[1], TokenKind::Int);
        assert_eq!(kinds("<?php 0b101")[1], TokenKind::Int);
        assert_eq!(kinds("<?php 1_000")[1], TokenKind::Int);
        assert_eq!(kinds("<?php 1.5e3")[1], TokenKind::Float);
        assert_eq!(kinds("<?php .5")[1], TokenKind::Float);
    }

    #[test]
    fn casts_and_operators() {
        assert_eq!(kinds("<?php (int)$x")[1], TokenKind::Cast(Cast::Int));
        assert_eq!(kinds("<?php (array)$x")[1], TokenKind::Cast(Cast::Array));
        assert!(kinds("<?php $a <=> $b").contains(&TokenKind::Spaceship));
        assert!(kinds("<?php $a ??= 1").contains(&TokenKind::CoalesceAssign));
        assert!(kinds("<?php $a ?-> b").contains(&TokenKind::NullsafeArrow));
    }

    #[test]
    fn keywords_and_contextual() {
        assert!(kinds("<?php fn() => 1").contains(&TokenKind::Keyword(Keyword::Fn)));
        assert!(kinds("<?php match($x){}").contains(&TokenKind::Keyword(Keyword::Match)));
        assert!(kinds("<?php readonly int $a").contains(&TokenKind::Keyword(Keyword::Readonly)));
        assert!(kinds("<?php yield from $g").contains(&TokenKind::Keyword(Keyword::YieldFrom)));
        // `int` is an identifier, not a keyword, matching PHP.
        assert!(kinds("<?php int $a").contains(&TokenKind::Identifier));
    }

    #[test]
    fn keyword_after_object_operator_is_identifier() {
        // `$o->class` — `class` is a property name (T_STRING), not a keyword.
        let ks = kinds("<?php $o->class;");
        assert!(ks.contains(&TokenKind::Identifier));
        assert!(!ks.contains(&TokenKind::Keyword(Keyword::Class)));
    }

    // --- differential oracle: our boundaries vs PHP's token_get_all ---------

    fn php_token_ends(src: &str) -> Option<Vec<usize>> {
        let script = "$c=stream_get_contents(STDIN);$o=0;\
            foreach(token_get_all($c) as $t){$o+=is_array($t)?strlen($t[1]):strlen($t);echo $o,\"\\n\";}";
        let mut child = Command::new("php")
            .args(["-r", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(src.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.parse().ok())
                .collect(),
        )
    }

    fn our_token_ends(src: &str) -> Vec<usize> {
        lex(src)
            .iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| t.span.end)
            .collect()
    }

    /// Compares byte boundaries against PHP's own tokenizer. Skips (does not fail)
    /// when `php` isn't installed. Snippets are scripting-mode-only plus simple
    /// interpolation, where we intentionally match PHP exactly. (In-string
    /// offsets like `"$a[0]"` are a documented divergence and excluded.)
    #[test]
    fn matches_php_token_boundaries() {
        let snippets = [
            "<?php $a = 1 + 2 * 3;",
            "<?php  \nfunction f(int $x): int { return $x; }",
            "<?php (int)$x; (array) $y; (object)$z;",
            "<?php $a <=> $b; $c ??= 1; $d ** 2; $e ?-> f;",
            "<?php 0x1F; 0b101; 0o17; 017; 1_000; 1.5e3; .5; 42;",
            "<?php fn() => 1; match($x){ 1 => 2 }; readonly int $a; enum E {}",
            "<?php $o->class; $o?->list; $o->prop;",
            "<?php \"a $b c $d e\"; 'single'; \"plain\";",
            "<?php $x; ?>\nHELLO<?php $y;",
            "<?php #[Route('/x')] function g() {}",
            "<?php yield from $gen; throw new E();",
        ];
        let Some(_) = php_token_ends("<?php 1;") else {
            eprintln!("skipping differential test: php not available");
            return;
        };
        for src in snippets {
            let ours = our_token_ends(src);
            let theirs = php_token_ends(src).expect("php tokenize");
            assert_eq!(ours, theirs, "boundary mismatch for {src:?}");
        }
    }
}

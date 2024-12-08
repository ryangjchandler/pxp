use std::collections::{HashMap, VecDeque};

use pxp_ast::*;
use pxp_bytestring::ByteString;
use pxp_diagnostics::{Diagnostic, Severity};
use pxp_lexer::Lexer;
use pxp_span::Span;
use pxp_token::{OwnedToken, Token, TokenKind};

use crate::{internal::identifiers::is_soft_reserved_identifier, ParserDiagnostic};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NamespaceType {
    Braced,
    Unbraced,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Scope {
    Namespace(ByteString),
    BracedNamespace(Option<ByteString>),
}

#[derive(Debug)]
pub struct State<'a> {
    // Unique identifier for each node.
    id: u32,

    // Scope Tracking
    pub stack: VecDeque<Scope>,
    pub imports: HashMap<UseKind, HashMap<ByteString, ByteString>>,
    pub namespace_type: Option<NamespaceType>,
    pub attributes: Vec<AttributeGroup>,
    comments: Vec<Comment>,
    docblock: bool,

    // Token Stream
    lexer: Lexer<'a>,

    // Diagnostics
    pub diagnostics: Vec<Diagnostic<ParserDiagnostic>>,
}

impl<'a> State<'a> {
    pub fn new(lexer: Lexer<'a>) -> Self {
        let mut imports = HashMap::new();
        imports.insert(UseKind::Normal, HashMap::new());
        imports.insert(UseKind::Function, HashMap::new());
        imports.insert(UseKind::Const, HashMap::new());

        let mut this = Self {
            stack: VecDeque::with_capacity(32),
            namespace_type: None,
            attributes: vec![],
            imports,
            comments: vec![],
            docblock: false,

            id: 0,

            lexer,

            diagnostics: vec![],
        };

        this.collect_comments();

        this
    }

    /// Move cursor to next token.
    ///
    /// Comments are collected.
    pub fn next(&mut self) {
        self.lexer.next();
        self.collect_comments();
    }

    /// Get current token.
    pub fn current(&'a self) -> Token<'a> {
        self.lexer.current()
    }

    /// Get previous token.
    pub fn previous(&'a self) -> Token<'a> {
        self.lexer.previous().unwrap_or_else(|| self.current())
    }

    /// Peek next token.
    ///
    /// All comments are skipped.
    pub fn peek(&'a mut self) -> Token<'a> {
        self.lexer.peek()
    }

    /// Peek nth+1 token.
    ///
    /// All comments are skipped.
    pub const fn lookahead(&self, n: usize) -> &'a Token {
        self.peek_nth(n + 1)
    }

    /// Peek nth token.
    ///
    /// All comments are skipped.
    #[inline(always)]
    const fn peek_nth(&self, n: usize) -> &'a Token {
        let mut cursor = self.cursor + 1;
        let mut target = 1;
        loop {
            if cursor >= self.length {
                return &self.tokens[self.length - 1];
            }

            let current = &self.tokens[cursor];

            if matches!(
                current.kind,
                TokenKind::SingleLineComment
                    | TokenKind::MultiLineComment
                    | TokenKind::HashMarkComment
            ) {
                cursor += 1;
                continue;
            }

            if target == n {
                return current;
            }

            target += 1;
            cursor += 1;
        }
    }

    pub const fn is_in_docblock(&self) -> bool {
        self.docblock
    }

    pub fn enter_docblock(&mut self) {
        self.docblock = true;
    }

    pub fn exit_docblock(&mut self) {
        self.docblock = false;
    }

    /// Check if current token is EOF.
    pub fn is_eof(&self) -> bool {
        self.current().kind == TokenKind::Eof
    }

    pub fn skip_doc_eol(&mut self) {
        if self.current().kind == TokenKind::PhpDocEol {
            self.next();
        }

        while self.current().kind == TokenKind::PhpDocHorizontalWhitespace {
            self.next();
        }
    }

    fn collect_comments(&mut self) {
        loop {
            if self.is_eof() {
                break;
            }

            let current = self.current();

            if !matches!(
                current.kind,
                TokenKind::SingleLineComment
                    | TokenKind::MultiLineComment
                    | TokenKind::HashMarkComment
                    | TokenKind::DocBlockComment
                    | TokenKind::OpenPhpDoc,
            ) {
                break;
            }

            let current = current.to_owned();

            let id = self.id();
            let comment_id = self.id();
            let (comment, move_forward) = match &current {
                OwnedToken {
                    kind: TokenKind::SingleLineComment,
                    span,
                    symbol,
                } => (
                    Comment {
                        id,
                        span: *span,
                        kind: CommentKind::SingleLine(SingleLineComment {
                            id: comment_id,
                            span: *span,
                            content: symbol.clone(),
                        }),
                    },
                    true,
                ),
                OwnedToken {
                    kind: TokenKind::MultiLineComment,
                    span,
                    symbol,
                } => (
                    Comment {
                        id,
                        span: *span,
                        kind: CommentKind::MultiLine(MultiLineComment {
                            id: comment_id,
                            span: *span,
                            content: symbol.clone(),
                        }),
                    },
                    true,
                ),
                OwnedToken {
                    kind: TokenKind::HashMarkComment,
                    span,
                    symbol,
                } => (
                    Comment {
                        id,
                        span: *span,
                        kind: CommentKind::HashMark(HashMarkComment {
                            id: comment_id,
                            span: *span,
                            content: symbol.clone(),
                        }),
                    },
                    true,
                ),
                #[cfg(not(feature = "docblocks"))]
                OwnedToken {
                    kind: TokenKind::DocBlockComment,
                    span,
                    symbol,
                } => (
                    Comment {
                        id,
                        span: *span,
                        kind: CommentKind::DocBlock(DocBlockComment {
                            id: comment_id,
                            span: *span,
                            content: symbol.clone(),
                        }),
                    },
                    true,
                ),
                #[cfg(feature = "docblocks")]
                Token {
                    kind: TokenKind::OpenPhpDoc,
                    ..
                } => {
                    let docblock = crate::internal::docblock::docblock(self);

                    (
                        Comment {
                            id,
                            span: docblock.span,
                            kind: CommentKind::DocBlock(docblock),
                        },
                        false,
                    )
                }
                _ => unreachable!(),
            };

            self.comments.push(comment);

            if move_forward {
                self.next();
            }
        }
    }

    pub fn comments(&mut self) -> CommentGroup {
        let mut comments = vec![];

        std::mem::swap(&mut self.comments, &mut comments);

        CommentGroup {
            id: self.id(),
            comments: comments.clone(),
        }
    }

    #[inline(always)]
    pub fn id(&mut self) -> u32 {
        self.id += 1;
        self.id
    }

    pub fn attribute(&mut self, attr: AttributeGroup) {
        self.attributes.push(attr);
    }

    pub fn get_attributes(&mut self) -> Vec<AttributeGroup> {
        let mut attributes = vec![];

        std::mem::swap(&mut self.attributes, &mut attributes);

        attributes
    }

    /// Return the namespace type used in the current state
    ///
    /// The namespace type is retrieve from the last entered
    /// namespace scope.
    ///
    /// Note: even when a namespace scope is exited, the namespace type
    /// is retained, until the next namespace scope is entered.
    pub fn namespace_type(&self) -> Option<&NamespaceType> {
        self.namespace_type.as_ref()
    }

    pub fn namespace(&self) -> Option<&Scope> {
        self.stack.iter().next()
    }

    pub fn maybe_resolve_identifier(&mut self, token: &Token, kind: UseKind) -> Name {
        let symbol = token.symbol;

        let part = match &token.kind {
            TokenKind::Identifier | TokenKind::Enum | TokenKind::From => {
                token.symbol.to_bytestring()
            }
            TokenKind::QualifiedIdentifier => {
                let bytestring = token.symbol.to_bytestring();
                let parts = bytestring.split(|c| *c == b'\\').collect::<Vec<_>>();

                ByteString::from(parts.first().unwrap().to_vec())
            }
            _ if is_soft_reserved_identifier(&token.kind) => token.symbol.to_bytestring(),
            _ => unreachable!(),
        };

        let id = self.id();
        let map = self.imports.get(&kind).unwrap();

        // We found an import that matches the first part of the identifier, so we can resolve it.
        if let Some(imported) = map.get(&part) {
            match &token.kind {
                TokenKind::Identifier | TokenKind::From | TokenKind::Enum => {
                    Name::resolved(id, imported.clone(), symbol.to_bytestring(), token.span)
                }
                TokenKind::QualifiedIdentifier => {
                    // Qualified identifiers might be aliased, so we need to take the full un-aliased import and
                    // concatenate that with everything after the first part of the qualified identifier.
                    let bytestring = symbol.clone();
                    let parts = bytestring.splitn(2, |c| *c == b'\\').collect::<Vec<_>>();
                    let rest = parts[1].to_vec().into();
                    let coagulated = imported.coagulate(&[rest], Some(b"\\"));

                    Name::resolved(id, coagulated, symbol.to_bytestring(), token.span)
                }
                _ => unreachable!(),
            }
        // We didn't find an import, but since we're trying to resolve the name of a class like, we can
        // follow PHP's name resolution rules and just prepend the current namespace.
        //
        // Additionally, if the name we're trying to resolve is qualified, then PHP's name resolution rules say that
        // we should just prepend the current namespace if the import map doesn't contain the first part.
        } else if kind == UseKind::Normal || token.kind == TokenKind::QualifiedIdentifier {
            Name::resolved(
                id,
                self.join_with_namespace(&symbol.to_bytestring()),
                symbol.to_bytestring(),
                token.span,
            )
        // Unqualified names in the global namespace can be resolved without any imports, since we can
        // only be referencing something else inside of the global namespace.
        } else if (kind == UseKind::Function || kind == UseKind::Const)
            && token.kind == TokenKind::Identifier
            && self.namespace().is_none()
        {
            Name::resolved(
                id,
                symbol.to_bytestring(),
                symbol.to_bytestring(),
                token.span,
            )
        } else {
            Name::unresolved(id, symbol.to_bytestring(), token.kind.into(), token.span)
        }
    }

    pub fn add_prefixed_import(
        &mut self,
        kind: &UseKind,
        prefix: ByteString,
        name: ByteString,
        alias: Option<ByteString>,
    ) {
        let coagulated = prefix.coagulate(&[name], Some(b"\\"));

        self.add_import(kind, coagulated, alias);
    }

    pub fn add_import(&mut self, kind: &UseKind, name: ByteString, alias: Option<ByteString>) {
        // We first need to check if the alias has been provided, and if not, create a new
        // symbol using the last part of the name.
        let alias = match alias {
            Some(alias) => alias,
            None => {
                let bytestring = name.clone();
                let parts = bytestring.split(|c| *c == b'\\').collect::<Vec<_>>();
                let last = parts.last().unwrap();

                ByteString::new(last.to_vec())
            }
        };

        // Then we can insert the import into the hashmap.
        self.imports.get_mut(kind).unwrap().insert(alias, name);
    }

    pub fn strip_leading_namespace_qualifier(&mut self, symbol: &ByteString) -> ByteString {
        if symbol.starts_with(b"\\") {
            ByteString::from(&symbol[1..])
        } else {
            symbol.clone()
        }
    }

    pub fn join_with_namespace(&mut self, name: &ByteString) -> ByteString {
        match self.namespace() {
            Some(Scope::Namespace(namespace)) => namespace.coagulate(&[name.clone()], Some(b"\\")),
            Some(Scope::BracedNamespace(Some(namespace))) => {
                namespace.coagulate(&[name.clone()], Some(b"\\"))
            }
            _ => name.clone(),
        }
    }

    pub fn previous_scope(&self) -> Option<&Scope> {
        self.stack.get(self.stack.len() - 2)
    }

    pub fn diagnostic(&mut self, kind: ParserDiagnostic, severity: Severity, span: Span) {
        self.diagnostics.push(Diagnostic::new(kind, severity, span));
    }

    pub fn enter(&mut self, scope: Scope) {
        match &scope {
            Scope::Namespace(_) => {
                self.namespace_type = Some(NamespaceType::Unbraced);
            }
            Scope::BracedNamespace(_) => {
                self.namespace_type = Some(NamespaceType::Braced);
            }
        }

        self.stack.push_back(scope);
    }

    pub fn exit(&mut self) {
        self.stack.pop_back();
    }
}

/// A byte-offset range into the original source buffer.
///
/// Spans are the backbone of the transpiler: the AST/token stream only needs to
/// know *where* each construct lives, and everything we don't transform is copied
/// verbatim from the source. That makes the source buffer itself the lossless
/// record — we never re-serialize untouched code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    /// The source text this span covers. Callers only ever slice on token
    /// boundaries, which the lexer guarantees to be UTF-8 aligned.
    pub fn as_str<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

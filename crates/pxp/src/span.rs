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

    /// The source bytes this span covers (PHP source isn't guaranteed UTF-8).
    pub fn slice<'a>(&self, src: &'a [u8]) -> crate::bytestr::ByteStr<'a> {
        crate::bytestr::ByteStr::new(&src[self.start..self.end])
    }
}

//! Byte-string types for PHP source.
//!
//! PHP source is a byte stream with no guaranteed encoding — identifiers may even
//! contain bytes `\x80..\xff`. So we never store source text as `String`/`&str`
//! (which would force UTF-8 and reject legacy files). Instead:
//!
//! * [`ByteStr`] — a borrowed `&[u8]` view (like `&str`, but encoding-agnostic).
//! * [`ByteString`] — an owned byte string, used for names kept in the AST.
//!
//! Both offer the handful of `&str`-like helpers we actually need (ASCII-case
//! comparison, comparison against string literals, lossy decoding *for display
//! only*). Emit never decodes — it splices raw bytes — so output is byte-exact.

use std::borrow::Cow;
use std::fmt;

/// A borrowed view of source bytes. Cheap to copy — it just wraps a `&[u8]`.
#[derive(Clone, Copy)]
pub struct ByteStr<'a>(&'a [u8]);

impl<'a> ByteStr<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        ByteStr(bytes)
    }

    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }

    pub fn len(self) -> usize {
        self.0.len()
    }

    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    pub fn to_owned(self) -> ByteString {
        ByteString(self.0.to_vec())
    }

    /// The bytes with the first one dropped (e.g. strip the `$` from a variable).
    pub fn without_first(self) -> ByteStr<'a> {
        ByteStr(self.0.split_first().map(|(_, rest)| rest).unwrap_or(&[]))
    }

    pub fn eq_ignore_ascii_case(self, other: &[u8]) -> bool {
        self.0.eq_ignore_ascii_case(other)
    }

    /// Lossily decode to UTF-8 — for diagnostics/display only, never for emit.
    pub fn to_str_lossy(self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.0)
    }
}

impl PartialEq<str> for ByteStr<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl PartialEq<&str> for ByteStr<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl fmt::Debug for ByteStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_str_lossy())
    }
}

impl fmt::Display for ByteStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_str_lossy())
    }
}

/// An owned byte string — identifiers and other names stored in the AST.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ByteString(Vec<u8>);

impl ByteString {
    pub fn new() -> Self {
        ByteString(Vec::new())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn as_byte_str(&self) -> ByteStr<'_> {
        ByteStr(&self.0)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    pub fn to_str_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

impl From<&[u8]> for ByteString {
    fn from(bytes: &[u8]) -> Self {
        ByteString(bytes.to_vec())
    }
}

impl From<Vec<u8>> for ByteString {
    fn from(bytes: Vec<u8>) -> Self {
        ByteString(bytes)
    }
}

impl From<&str> for ByteString {
    fn from(s: &str) -> Self {
        ByteString(s.as_bytes().to_vec())
    }
}

impl From<ByteStr<'_>> for ByteString {
    fn from(b: ByteStr<'_>) -> Self {
        b.to_owned()
    }
}

/// Borrow as `[u8]` so a `HashSet<ByteString>` can be queried with a `&[u8]`.
impl std::borrow::Borrow<[u8]> for ByteString {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl PartialEq<str> for ByteString {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl PartialEq<&str> for ByteString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl fmt::Debug for ByteString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_str_lossy())
    }
}

impl fmt::Display for ByteString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_str_lossy())
    }
}

/// Maps byte offsets to 1-based line/column and back.
///
/// Built once per file (source and generated), it translates positions in either
/// direction to complement the parser's byte spans.
pub struct LineIndex {
    /// Byte offset of the first character of each line. `line_starts[0] == 0`.
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(src: &[u8]) -> Self {
        let mut line_starts = vec![0];
        for (i, &b) in src.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// Number of lines. A trailing newline yields a final empty line, matching
    /// how editors and PHP stack traces count.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// 1-based line number containing `byte`.
    pub fn line_of(&self, byte: usize) -> u32 {
        // Count of line starts at or before `byte`; always >= 1 since starts[0]==0.
        self.line_starts.partition_point(|&s| s <= byte) as u32
    }

    /// Byte offset where 1-based `line` begins.
    pub fn line_start(&self, line: u32) -> usize {
        self.line_starts[(line as usize - 1).min(self.line_starts.len() - 1)]
    }

    /// 1-based (line, column) for `byte`.
    pub fn line_col(&self, byte: usize) -> (u32, u32) {
        let line = self.line_of(byte);
        let col = (byte - self.line_start(line)) as u32 + 1;
        (line, col)
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn lines_and_columns() {
        let idx = LineIndex::new(b"<?php\nabc\n");
        assert_eq!(idx.line_count(), 3); // "<?php", "abc", ""
        assert_eq!(idx.line_of(0), 1);
        assert_eq!(idx.line_of(6), 2); // 'a'
        assert_eq!(idx.line_col(7), (2, 2)); // 'b'
        assert_eq!(idx.line_start(2), 6);
    }
}

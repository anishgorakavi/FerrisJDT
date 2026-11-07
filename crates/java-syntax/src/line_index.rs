/// A zero-based line/character position. Characters are counted in UTF-16 code
/// units, matching the LSP default `positionEncoding`, which differs from Rust
/// byte offsets for any non-ASCII source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    pub fn new(line: u32, character: u32) -> Self {
        Position { line, character }
    }
}

/// Maps between byte offsets and LSP positions for one document revision.
///
/// Lines that are pure ASCII — the overwhelming majority — are flagged so
/// their column conversion is plain arithmetic with no scanning.
pub struct LineIndex {
    /// Byte offset of the start of each line.
    line_starts: Vec<u32>,
    /// Parallel to `line_starts`: whether that line contains only ASCII.
    ascii_only: Vec<bool>,
    len: u32,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        let mut ascii_only = Vec::new();
        let mut line_is_ascii = true;
        for (offset, byte) in text.bytes().enumerate() {
            if byte >= 0x80 {
                line_is_ascii = false;
            }
            if byte == b'\n' {
                line_starts.push(offset as u32 + 1);
                ascii_only.push(line_is_ascii);
                line_is_ascii = true;
            }
        }
        ascii_only.push(line_is_ascii);
        LineIndex {
            line_starts,
            ascii_only,
            len: text.len() as u32,
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn line_start(&self, line: u32) -> Option<u32> {
        self.line_starts.get(line as usize).copied()
    }

    /// Byte range of a line, excluding its terminator.
    pub fn line_range(&self, line: u32) -> Option<(u32, u32)> {
        let start = self.line_start(line)?;
        let end = match self.line_starts.get(line as usize + 1) {
            // Step back over the newline, and over a preceding CR if present.
            Some(&next) => next.saturating_sub(1),
            None => self.len,
        };
        Some((start, end.max(start)))
    }

    /// Converts an LSP position to a byte offset, clamping out-of-range input
    /// rather than failing: editors legitimately send a position one past the
    /// end of a line, and a stale position must not abort a request.
    pub fn offset(&self, text: &str, position: Position) -> u32 {
        let line = position.line.min(self.line_count().saturating_sub(1));
        let (start, end) = self.line_range(line).unwrap_or((self.len, self.len));
        if self.ascii_only.get(line as usize).copied().unwrap_or(false) {
            return (start + position.character).min(end);
        }
        let mut utf16 = 0u32;
        let mut offset = start;
        for ch in text[start as usize..end as usize].chars() {
            if utf16 >= position.character {
                break;
            }
            utf16 += ch.len_utf16() as u32;
            offset += ch.len_utf8() as u32;
        }
        offset.min(end)
    }

    /// Converts a byte offset to an LSP position, clamping to the document.
    pub fn position(&self, text: &str, offset: u32) -> Position {
        let offset = offset.min(self.len);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(next) => next - 1,
        };
        let start = self.line_starts[line];
        let character = if self.ascii_only.get(line).copied().unwrap_or(false) {
            offset - start
        } else {
            text[start as usize..offset as usize]
                .chars()
                .map(|c| c.len_utf16() as u32)
                .sum()
        };
        Position {
            line: line as u32,
            character,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_ascii_positions() {
        let text = "class A {\n    int x;\n}\n";
        let index = LineIndex::new(text);
        assert_eq!(index.line_count(), 4);
        assert_eq!(index.offset(text, Position::new(1, 4)), 14);
        assert_eq!(index.position(text, 14), Position::new(1, 4));
        assert_eq!(index.position(text, 0), Position::new(0, 0));
    }

    #[test]
    fn counts_utf16_units_for_astral_characters() {
        // The emoji is one char, four UTF-8 bytes, two UTF-16 units.
        let text = "// \u{1F980}x\nint y;";
        let index = LineIndex::new(text);
        // Column 5 is past the emoji (3 ASCII + 2 units) and lands on `x`.
        assert_eq!(index.offset(text, Position::new(0, 5)), 7);
        assert_eq!(index.position(text, 7), Position::new(0, 5));
        assert_eq!(index.position(text, 3), Position::new(0, 3));
    }

    #[test]
    fn clamps_positions_beyond_the_document() {
        let text = "a\nbb";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 99)), 1);
        assert_eq!(index.offset(text, Position::new(99, 0)), 2);
        assert_eq!(index.position(text, 999), Position::new(1, 2));
    }

    #[test]
    fn handles_empty_and_newline_only_documents() {
        let index = LineIndex::new("");
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.offset("", Position::new(0, 0)), 0);
        let index = LineIndex::new("\n");
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.position("\n", 1), Position::new(1, 0));
    }
}

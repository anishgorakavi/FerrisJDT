//! Bounds-checked byte reading, and JVM modified-UTF-8 decoding.

use crate::error::{ClassFileError, Result};

/// Bounds-checked big-endian cursor. Every read returns `Err` rather than
/// panicking so that a malformed class file cannot take down the server.
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(ClassFileError::Truncated(self.pos))?;
        if end > self.bytes.len() {
            return Err(ClassFileError::Truncated(self.pos));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
}

/// Decodes JVM "modified UTF-8" (JVMS 4.4.7): NUL is two bytes and supplementary
/// characters arrive as a CESU-8 surrogate pair. Invalid sequences become U+FFFD
/// instead of an error, because a single bad name must not discard a whole JAR.
pub fn decode_modified_utf8(bytes: &[u8]) -> String {
    // The overwhelmingly common case is plain ASCII, which needs no work.
    if bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0x01..=0x7f => {
                out.push(b as char);
                i += 1;
            }
            0xc0..=0xdf if i + 1 < bytes.len() => {
                let c = ((b as u32 & 0x1f) << 6) | (bytes[i + 1] as u32 & 0x3f);
                out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                i += 2;
            }
            0xe0..=0xef if i + 2 < bytes.len() => {
                let c = ((b as u32 & 0x0f) << 12)
                    | ((bytes[i + 1] as u32 & 0x3f) << 6)
                    | (bytes[i + 2] as u32 & 0x3f);
                // A high surrogate followed by an encoded low surrogate is one
                // supplementary character split across six bytes.
                if (0xd800..0xdc00).contains(&c) && i + 5 < bytes.len() && bytes[i + 3] == 0xed {
                    let lo = ((bytes[i + 3] as u32 & 0x0f) << 12)
                        | ((bytes[i + 4] as u32 & 0x3f) << 6)
                        | (bytes[i + 5] as u32 & 0x3f);
                    if (0xdc00..0xe000).contains(&lo) {
                        let combined = 0x10000 + ((c - 0xd800) << 10) + (lo - 0xdc00);
                        out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                        i += 6;
                        continue;
                    }
                }
                out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                i += 3;
            }
            _ => {
                out.push('\u{fffd}');
                i += 1;
            }
        }
    }
    out
}

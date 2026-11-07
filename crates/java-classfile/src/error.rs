//! The error type for class file and archive decoding.

use std::io;

/// Every decoding failure is reported, never panicked on: class files and JARs
/// are untrusted input that may be truncated, cyclic, or deliberately malformed.
#[derive(Debug, thiserror::Error)]
pub enum ClassFileError {
    #[error("unexpected end of input at offset {0}")]
    Truncated(usize),
    #[error("bad magic: expected 0xCAFEBABE, found {0:#010x}")]
    BadMagic(u32),
    #[error("constant pool index {0} is out of range")]
    BadConstantIndex(u16),
    #[error("constant pool entry {index} is a {found}, expected {expected}")]
    WrongConstantKind {
        index: u16,
        found: &'static str,
        expected: &'static str,
    },
    #[error("unknown constant pool tag {0}")]
    UnknownTag(u8),
    #[error("malformed type descriptor {0:?}")]
    BadDescriptor(String),
    #[error("not a zip archive: end-of-central-directory record not found")]
    NotAnArchive,
    #[error("malformed zip archive: {0}")]
    BadArchive(&'static str),
    #[error("unsupported compression method {0}")]
    UnsupportedCompression(u16),
    #[error("entry is larger than the {0} byte limit")]
    EntryTooLarge(u64),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, ClassFileError>;

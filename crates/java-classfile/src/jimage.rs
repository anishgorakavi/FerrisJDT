//! A minimal, read-only reader for the JDK's `lib/modules` jimage container.
//!
//! JDK 9 and later keep the platform's classes in this file rather than in `rt.jar`. Only the
//! newest release lives here alone: `ct.sym` records older releases but not the JDK's own, so
//! APIs added in the running JDK's release (such as `List.getFirst`) are readable only from the
//! jimage. The index (header, hash tables, location attributes, strings) is held in memory, about
//! 1.6 MB for a full JDK, and class bytes are read from the file on demand. Resources compressed
//! with `jlink --compress` are not supported and are reported as an error, never guessed at.
//!
//! Layout, all little-endian: a 28-byte header; `table_length` redirect entries (i32); the same
//! number of location offsets (u32); the location attribute bytes; the string table; then the
//! resource data. A resource is found by a perfect hash of `/module/parent/base.extension`.

use crate::error::{ClassFileError, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

const MAGIC: u32 = 0xCAFE_DADA;
const HEADER_LEN: usize = 28;
const HASH_MULTIPLIER: u32 = 0x0100_0193;
/// Bounds on the index so a corrupt header cannot request an absurd allocation.
const MAX_TABLE_LENGTH: u32 = 4_000_000;
const MAX_REGION: u32 = 256 * 1024 * 1024;
/// No class file approaches this; it bounds a read driven by an untrusted size field.
const MAX_RESOURCE: u64 = 64 * 1024 * 1024;

const ATTRIBUTE_MODULE: u8 = 1;
const ATTRIBUTE_PARENT: u8 = 2;
const ATTRIBUTE_BASE: u8 = 3;
const ATTRIBUTE_EXTENSION: u8 = 4;
const ATTRIBUTE_OFFSET: u8 = 5;
const ATTRIBUTE_COMPRESSED: u8 = 6;
const ATTRIBUTE_UNCOMPRESSED: u8 = 7;

/// One decoded location: where a resource's name parts and bytes are.
#[derive(Default)]
struct Location {
    module: u64,
    parent: u64,
    base: u64,
    extension: u64,
    offset: u64,
    compressed: u64,
    size: u64,
}

pub struct JImage {
    /// Header, tables, locations and strings, concatenated as in the file.
    index: Vec<u8>,
    table_length: usize,
    locations_start: usize,
    strings_start: usize,
    strings_len: usize,
    /// File offset of the first resource byte.
    data_start: u64,
    file: Mutex<File>,
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// The jimage string hash: a multiply-xor over the UTF-8 bytes, kept non-negative.
fn hash(name: &str, seed: u32) -> u32 {
    let mut value = seed;
    for byte in name.bytes() {
        value = value.wrapping_mul(HASH_MULTIPLIER) ^ u32::from(byte);
    }
    value & 0x7FFF_FFFF
}

impl JImage {
    /// Opens `lib/modules` and loads its index.
    pub fn open(path: &Path) -> Result<JImage> {
        let mut file = File::open(path)?;
        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        let magic = le32(&header, 0).ok_or(ClassFileError::Truncated(0))?;
        if magic != MAGIC {
            return Err(ClassFileError::BadMagic(magic));
        }
        let field = |at: usize| le32(&header, at).ok_or(ClassFileError::Truncated(at));
        let table_length = field(16)?;
        let locations_size = field(20)?;
        let strings_size = field(24)?;
        if table_length > MAX_TABLE_LENGTH
            || locations_size > MAX_REGION
            || strings_size > MAX_REGION
        {
            return Err(ClassFileError::BadArchive(
                "jimage index is implausibly large",
            ));
        }
        let table_length = table_length as usize;
        let index_len =
            HEADER_LEN + table_length * 8 + locations_size as usize + strings_size as usize;
        file.seek(SeekFrom::Start(0))?;
        let mut index = vec![0u8; index_len];
        file.read_exact(&mut index)?;
        Ok(JImage {
            table_length,
            locations_start: HEADER_LEN + table_length * 8,
            strings_start: HEADER_LEN + table_length * 8 + locations_size as usize,
            strings_len: strings_size as usize,
            data_start: index_len as u64,
            index,
            file: Mutex::new(file),
        })
    }

    /// A NUL-terminated string from the string table.
    fn string(&self, offset: u64) -> Option<&str> {
        let offset = usize::try_from(offset)
            .ok()
            .filter(|o| *o < self.strings_len)?;
        let region =
            &self.index[self.strings_start + offset..self.strings_start + self.strings_len];
        let end = region.iter().position(|b| *b == 0)?;
        std::str::from_utf8(&region[..end]).ok()
    }

    /// Decodes the attribute run at `offset` into the locations area.
    fn location(&self, offset: u32) -> Option<Location> {
        let mut at = self.locations_start.checked_add(offset as usize)?;
        let end = self.strings_start;
        let mut found = Location::default();
        loop {
            let byte = *self.index.get(at)?;
            if at >= end {
                return None;
            }
            let kind = byte >> 3;
            if kind == 0 {
                return Some(found);
            }
            let length = usize::from(byte & 7) + 1;
            let bytes = self.index.get(at + 1..at + 1 + length)?;
            let value = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
            match kind {
                ATTRIBUTE_MODULE => found.module = value,
                ATTRIBUTE_PARENT => found.parent = value,
                ATTRIBUTE_BASE => found.base = value,
                ATTRIBUTE_EXTENSION => found.extension = value,
                ATTRIBUTE_OFFSET => found.offset = value,
                ATTRIBUTE_COMPRESSED => found.compressed = value,
                ATTRIBUTE_UNCOMPRESSED => found.size = value,
                _ => {}
            }
            at += 1 + length;
        }
    }

    /// Calls `visit(module, internal_name)` for every class, e.g. `("java.base", "java/lang/String")`.
    /// Resources that are not classes, and `module-info`, are skipped.
    pub fn for_each_class(&self, mut visit: impl FnMut(&str, &str)) {
        let offsets = HEADER_LEN + self.table_length * 4;
        for slot in 0..self.table_length {
            let Some(offset) = le32(&self.index, offsets + slot * 4) else {
                continue;
            };
            let Some(location) = self.location(offset) else {
                continue;
            };
            if self.string(location.extension) != Some("class") {
                continue;
            }
            let (Some(module), Some(parent), Some(base)) = (
                self.string(location.module),
                self.string(location.parent),
                self.string(location.base),
            ) else {
                continue;
            };
            if module.is_empty() || base == "module-info" {
                continue;
            }
            let internal = if parent.is_empty() {
                base.to_string()
            } else {
                format!("{parent}/{base}")
            };
            visit(module, &internal);
        }
    }

    /// Reads the class `internal` (such as `java/util/List`) of `module`, if present.
    pub fn read_class(&self, module: &str, internal: &str) -> Result<Option<Vec<u8>>> {
        let name = format!("/{module}/{internal}.class");
        let length = self.table_length as u32;
        if length == 0 {
            return Ok(None);
        }
        let redirect = le32(
            &self.index,
            HEADER_LEN + (hash(&name, HASH_MULTIPLIER) % length) as usize * 4,
        )
        .ok_or(ClassFileError::BadArchive(
            "jimage redirect table is truncated",
        ))? as i32;
        let slot = match redirect {
            0 => return Ok(None),
            value if value < 0 => (-1 - i64::from(value)) as u32,
            value => hash(&name, value as u32) % length,
        };
        if slot >= length {
            return Ok(None);
        }
        let Some(offset) = le32(
            &self.index,
            HEADER_LEN + self.table_length * 4 + slot as usize * 4,
        ) else {
            return Ok(None);
        };
        let Some(location) = self.location(offset) else {
            return Err(ClassFileError::BadArchive("jimage location is malformed"));
        };
        // A hash slot can belong to another resource: confirm the full name.
        let (parent, base) = (
            self.string(location.parent).unwrap_or(""),
            self.string(location.base).unwrap_or(""),
        );
        let found = if parent.is_empty() {
            base.to_string()
        } else {
            format!("{parent}/{base}")
        };
        if self.string(location.module) != Some(module)
            || found != internal
            || self.string(location.extension) != Some("class")
        {
            return Ok(None);
        }
        if location.compressed != 0 {
            return Err(ClassFileError::UnsupportedCompression(0));
        }
        if location.size > MAX_RESOURCE {
            return Err(ClassFileError::EntryTooLarge(MAX_RESOURCE));
        }
        let mut bytes = vec![0u8; location.size as usize];
        let mut file = self
            .file
            .lock()
            .map_err(|_| ClassFileError::BadArchive("jimage file lock poisoned"))?;
        file.seek(SeekFrom::Start(self.data_start + location.offset))?;
        file.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    }
}

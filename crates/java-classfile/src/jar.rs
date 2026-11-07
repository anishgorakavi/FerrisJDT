//! A minimal, read-only ZIP reader for JARs.
//!
//! The `zip` crate would also work, but its in-memory directory is far larger
//! than what this server needs: a 20k-entry `rt.jar` costs several megabytes of
//! per-entry structs. Here every entry name lives in one shared string arena
//! and each entry is a fixed 24-byte record, so indexing `rt.jar` costs about
//! 1 MB. Entries stay sorted by name and are found by binary search, which
//! avoids storing a second copy of every name in a hash map.

use crate::error::{ClassFileError, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Refuses to inflate anything larger than this, so a malicious archive cannot
/// exhaust memory through a decompression bomb. No real class file comes close.
const MAX_ENTRY_SIZE: u64 = 64 * 1024 * 1024;

const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

#[derive(Clone, Copy)]
struct Entry {
    name_start: u32,
    name_len: u32,
    local_offset: u64,
    compressed_size: u32,
    uncompressed_size: u32,
    method: u16,
}

pub struct Jar {
    path: PathBuf,
    /// All entry names concatenated; `Entry` holds ranges into this.
    names: String,
    /// Sorted by name so lookup is a binary search.
    entries: Vec<Entry>,
    file: Mutex<File>,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

impl Jar {
    pub fn open(path: &Path) -> Result<Jar> {
        let mut file = File::open(path)?;
        let size = file.seek(SeekFrom::End(0))?;
        let (cd_offset, cd_size, entry_count) = read_directory_location(&mut file, size)?;
        if cd_offset > size || cd_size > size {
            return Err(ClassFileError::BadArchive(
                "central directory lies outside the file",
            ));
        }
        let mut cd = vec![0u8; cd_size as usize];
        file.seek(SeekFrom::Start(cd_offset))?;
        file.read_exact(&mut cd)?;

        let mut names = String::new();
        let mut entries = Vec::with_capacity(entry_count.min(1 << 20) as usize);
        let mut at = 0usize;
        while at + 46 <= cd.len() {
            if le32(&cd, at) != CENTRAL_SIG {
                break;
            }
            let method = le16(&cd, at + 10);
            let mut compressed_size = le32(&cd, at + 20);
            let mut uncompressed_size = le32(&cd, at + 24);
            let name_len = le16(&cd, at + 28) as usize;
            let extra_len = le16(&cd, at + 30) as usize;
            let comment_len = le16(&cd, at + 32) as usize;
            let mut local_offset = le32(&cd, at + 42) as u64;
            let header_end = at + 46;
            let name_end = header_end + name_len;
            let extra_end = name_end + extra_len;
            let record_end = extra_end + comment_len;
            if record_end > cd.len() {
                return Err(ClassFileError::BadArchive("truncated central directory"));
            }
            let name = String::from_utf8_lossy(&cd[header_end..name_end]);
            // Zip64 replaces any field held at 0xFFFFFFFF, in a fixed order.
            if uncompressed_size == u32::MAX
                || compressed_size == u32::MAX
                || local_offset == u64::from(u32::MAX)
            {
                if let Some(z64) = find_extra(&cd[name_end..extra_end], 0x0001) {
                    let mut at64 = 0usize;
                    if uncompressed_size == u32::MAX && at64 + 8 <= z64.len() {
                        uncompressed_size = le64(z64, at64).min(u64::from(u32::MAX)) as u32;
                        at64 += 8;
                    }
                    if compressed_size == u32::MAX && at64 + 8 <= z64.len() {
                        compressed_size = le64(z64, at64).min(u64::from(u32::MAX)) as u32;
                        at64 += 8;
                    }
                    if local_offset == u64::from(u32::MAX) && at64 + 8 <= z64.len() {
                        local_offset = le64(z64, at64);
                    }
                }
            }
            if !name.ends_with('/') {
                let name_start = names.len() as u32;
                names.push_str(&name);
                entries.push(Entry {
                    name_start,
                    name_len: name.len() as u32,
                    local_offset,
                    compressed_size,
                    uncompressed_size,
                    method,
                });
            }
            at = record_end;
        }
        names.shrink_to_fit();
        entries.sort_by(|a, b| slice(&names, a).cmp(slice(&names, b)));
        entries.shrink_to_fit();
        Ok(Jar {
            path: path.to_path_buf(),
            names,
            entries,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.entries.iter().map(|e| slice(&self.names, e))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        let index = self
            .entries
            .binary_search_by(|e| slice(&self.names, e).cmp(name))
            .ok()?;
        self.entries.get(index)
    }

    /// Reads and decompresses one entry. Returns `None` when absent.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let entry = match self.find(name) {
            Some(e) => *e,
            None => return Ok(None),
        };
        if u64::from(entry.uncompressed_size) > MAX_ENTRY_SIZE {
            return Err(ClassFileError::EntryTooLarge(MAX_ENTRY_SIZE));
        }
        let mut file = self.file.lock().map_err(|_| {
            ClassFileError::BadArchive("archive handle poisoned by an earlier failure")
        })?;
        // The local header repeats the name and extra fields with lengths of
        // their own, so the data offset can only be computed by reading it.
        let mut header = [0u8; 30];
        file.seek(SeekFrom::Start(entry.local_offset))?;
        file.read_exact(&mut header)?;
        if le32(&header, 0) != LOCAL_SIG {
            return Err(ClassFileError::BadArchive("bad local file header"));
        }
        let skip = le16(&header, 26) as u64 + le16(&header, 28) as u64;
        file.seek(SeekFrom::Start(entry.local_offset + 30 + skip))?;
        let mut compressed = vec![0u8; entry.compressed_size as usize];
        file.read_exact(&mut compressed)?;
        drop(file);

        match entry.method {
            METHOD_STORE => Ok(Some(compressed)),
            METHOD_DEFLATE => {
                let mut out = Vec::with_capacity(entry.uncompressed_size as usize);
                flate2::read::DeflateDecoder::new(compressed.as_slice())
                    .take(MAX_ENTRY_SIZE)
                    .read_to_end(&mut out)?;
                Ok(Some(out))
            }
            other => Err(ClassFileError::UnsupportedCompression(other)),
        }
    }
}

fn slice<'a>(names: &'a str, entry: &Entry) -> &'a str {
    let start = entry.name_start as usize;
    &names[start..start + entry.name_len as usize]
}

fn find_extra(extra: &[u8], want_id: u16) -> Option<&[u8]> {
    let mut at = 0usize;
    while at + 4 <= extra.len() {
        let id = le16(extra, at);
        let len = le16(extra, at + 2) as usize;
        let body_end = at + 4 + len;
        if body_end > extra.len() {
            return None;
        }
        if id == want_id {
            return Some(&extra[at + 4..body_end]);
        }
        at = body_end;
    }
    None
}

/// Finds the central directory, following the Zip64 records when present.
/// Returns `(offset, size, entry_count)`.
fn read_directory_location(file: &mut File, size: u64) -> Result<(u64, u64, u64)> {
    // The EOCD sits within the last 64 KiB plus its own 22-byte header.
    let window = size.min(22 + u16::MAX as u64);
    let start = size - window;
    let mut tail = vec![0u8; window as usize];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| le32(&tail, i) == EOCD_SIG)
        .ok_or(ClassFileError::NotAnArchive)?;
    let mut count = le16(&tail, eocd + 10) as u64;
    let mut cd_size = le32(&tail, eocd + 12) as u64;
    let mut cd_offset = le32(&tail, eocd + 16) as u64;

    // Any field held at its maximum is stored in the Zip64 records instead.
    let saturated = cd_offset == u64::from(u32::MAX)
        || cd_size == u64::from(u32::MAX)
        || count == u64::from(u16::MAX);
    if saturated && eocd >= 20 && le32(&tail, eocd - 20) == ZIP64_LOCATOR_SIG {
        let z64_offset = le64(&tail, eocd - 20 + 8);
        let mut z64 = [0u8; 56];
        file.seek(SeekFrom::Start(z64_offset))?;
        file.read_exact(&mut z64)?;
        if le32(&z64, 0) != ZIP64_EOCD_SIG {
            return Err(ClassFileError::BadArchive(
                "bad zip64 end of central directory",
            ));
        }
        count = le64(&z64, 32);
        cd_size = le64(&z64, 40);
        cd_offset = le64(&z64, 48);
    }
    Ok((cd_offset, cd_size, count))
}

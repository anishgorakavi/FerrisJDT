//! Conversion between filesystem paths and `file:` URIs.
//!
//! `lsp-types` 0.97 models a URI as an opaque validated string and does not
//! convert paths. The `url` crate would, but it brings in IDNA tables that are
//! pure weight for a server that only ever handles local file paths, so the
//! conversion is done here and tested directly.

use lsp_types::Uri;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Characters that may appear unescaped in a path segment (RFC 3986
/// unreserved, plus the sub-delims that no implementation objects to).
fn is_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b'$' | b'+')
}

/// Builds a `file:` URI from an absolute path.
pub fn from_path(path: &Path) -> Option<Uri> {
    let text = path.to_str()?;
    if !text.starts_with('/') && !has_drive_letter(text) {
        // A relative path has no unambiguous URI.
        return None;
    }
    let mut out = String::with_capacity(text.len() + 16);
    out.push_str("file://");
    if has_drive_letter(text) {
        out.push('/');
    }
    for &byte in text.as_bytes() {
        if is_safe(byte) {
            out.push(byte as char);
        } else if byte == b'\\' {
            out.push('/');
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    Uri::from_str(&out).ok()
}

fn has_drive_letter(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Extracts the filesystem path from a `file:` URI. Returns `None` for any
/// other scheme, which is how requests for non-file documents are declined.
pub fn to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;
    // An authority of `localhost` is equivalent to an empty one; anything
    // else names a remote host this server cannot read.
    let rest = match rest.find('/') {
        Some(0) => rest,
        Some(at) if &rest[..at] == "localhost" => &rest[at..],
        None if rest.is_empty() => "/",
        _ => return None,
    };
    let decoded = percent_decode(rest)?;
    // A Windows URI is `/C:/path`; drop the leading slash.
    let decoded = if has_drive_letter(decoded.get(1..).unwrap_or("")) {
        decoded[1..].to_string()
    } else {
        decoded
    };
    Some(PathBuf::from(decoded))
}

fn percent_decode(text: &str) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_string());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_ordinary_paths() {
        for path in [
            "/Users/student/homework/BinarySearchTree.java",
            "/tmp/a/b/C.java",
            "/src/main/java/ds/Node.java",
        ] {
            let uri = from_path(Path::new(path)).expect("absolute path converts");
            assert_eq!(uri.as_str(), format!("file://{path}"));
            assert_eq!(to_path(&uri).unwrap(), PathBuf::from(path));
        }
    }

    #[test]
    fn escapes_and_restores_special_characters() {
        let path = "/tmp/my project/a b#c?d%e.java";
        let uri = from_path(Path::new(path)).unwrap();
        assert!(!uri.as_str().contains(' '), "{}", uri.as_str());
        assert!(uri.as_str().contains("%20"));
        assert_eq!(to_path(&uri).unwrap(), PathBuf::from(path));
    }

    #[test]
    fn round_trips_non_ascii_paths() {
        let path = "/tmp/\u{e9}t\u{e9}/caf\u{e9}.java";
        let uri = from_path(Path::new(path)).unwrap();
        assert_eq!(to_path(&uri).unwrap(), PathBuf::from(path));
    }

    #[test]
    fn rejects_relative_paths_and_other_schemes() {
        assert!(from_path(Path::new("relative/path.java")).is_none());
        let uri = Uri::from_str("http://example.com/x.java").unwrap();
        assert!(to_path(&uri).is_none());
        let uri = Uri::from_str("file://remotehost/x.java").unwrap();
        assert!(to_path(&uri).is_none());
    }

    #[test]
    fn accepts_a_localhost_authority() {
        let uri = Uri::from_str("file://localhost/tmp/A.java").unwrap();
        assert_eq!(to_path(&uri).unwrap(), PathBuf::from("/tmp/A.java"));
    }
}

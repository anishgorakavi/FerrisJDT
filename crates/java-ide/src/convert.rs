//! Conversions between the analysis layer's byte offsets and LSP positions.

use java_semantic::symbols::{FileId, Origin};
use java_semantic::Db;
use java_syntax::line_index::Position as SyntaxPosition;
use lsp_types::{Location, Position, Range, Uri};

pub fn to_syntax_position(position: Position) -> SyntaxPosition {
    SyntaxPosition {
        line: position.line,
        character: position.character,
    }
}

pub fn from_syntax_position(position: SyntaxPosition) -> Position {
    Position {
        line: position.line,
        character: position.character,
    }
}

/// Byte offset of an LSP position in a file, clamped to the document.
pub fn offset_of(db: &Db, file: FileId, position: Position) -> Option<u32> {
    let source = db.source(file)?;
    Some(source.offset_at(to_syntax_position(position)))
}

/// Converts a byte range in a file to an LSP range.
pub fn range_of(db: &Db, file: FileId, range: (u32, u32)) -> Option<Range> {
    let source = db.source(file)?;
    Some(Range {
        start: from_syntax_position(source.position_at(range.0)),
        end: from_syntax_position(source.position_at(range.1)),
    })
}

pub fn uri_of(db: &Db, file: FileId) -> Option<Uri> {
    crate::uri::from_path(db.path(file)?)
}

pub fn location_of(db: &Db, file: FileId, range: (u32, u32)) -> Option<Location> {
    Some(Location {
        uri: uri_of(db, file)?,
        range: range_of(db, file, range)?,
    })
}

/// A declaration's location, when it came from a file in the workspace.
/// Classpath declarations have no source range and yield `None`.
pub fn origin_location(db: &Db, origin: &Origin) -> Option<Location> {
    let Origin::Source {
        file, name_range, ..
    } = origin
    else {
        return None;
    };
    location_of(db, *file, *name_range)
}

/// A declaration's full range plus its name range, for `LocationLink`.
pub fn origin_link_ranges(db: &Db, origin: &Origin) -> Option<(Location, Range)> {
    let Origin::Source {
        file,
        range,
        name_range,
    } = origin
    else {
        return None;
    };
    let target = location_of(db, *file, *range)?;
    let selection = range_of(db, *file, *name_range)?;
    Some((target, selection))
}

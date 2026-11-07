//! Builds an analysis database from in-memory source files.
//!
//! Available only under the `fixture` feature, which the test targets enable.
//! A `$0` in a file's text marks the cursor; it is removed before the file is
//! written, and its byte offset is recorded.

use crate::db::Db;
use crate::symbols::FileId;
use project_model::{Config, Project};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub db: Db,
    /// File and offset of the `$0` marker, when one was given.
    pub cursor: Option<(FileId, u32)>,
}

impl Fixture {
    /// Writes the given files into a fresh directory and indexes them.
    pub fn new(files: &[(&str, &str)]) -> Fixture {
        Fixture::with_config(files, Config::default())
    }

    pub fn with_config(files: &[(&str, &str)], config: Config) -> Fixture {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("ferrisjdt-fixtures")
            .join(format!("{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture directory should be creatable");

        let mut cursors = Vec::new();
        for (name, text) in files {
            let path = root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            let (clean, cursor) = match text.find("$0") {
                Some(at) => (text.replacen("$0", "", 1), Some(at as u32)),
                None => (text.to_string(), None),
            };
            std::fs::write(&path, &clean).unwrap();
            if let Some(offset) = cursor {
                cursors.push((path, offset));
            }
        }

        let project = Project::load(&root, &config);
        let mut db = Db::new(project);
        for path in db.project().discover_source_files() {
            db.load_from_disk(&path);
        }
        let cursor = cursors
            .first()
            .and_then(|(path, offset)| db.file_id(path).map(|file| (file, *offset)));
        Fixture { root, db, cursor }
    }

    pub fn file(&self, name: &str) -> FileId {
        self.db
            .file_id(&self.root.join(name))
            .unwrap_or_else(|| panic!("fixture has no file named {name}"))
    }

    /// The recorded `$0` position.
    pub fn cursor(&self) -> (FileId, u32) {
        self.cursor.expect("fixture has no $0 marker")
    }

    /// Byte offset just after the first occurrence of `needle` in a file.
    pub fn after(&self, name: &str, needle: &str) -> u32 {
        let file = self.file(name);
        let text = self.db.text(file).unwrap();
        let at = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found in {name}"));
        (at + needle.len()) as u32
    }

    /// Byte offset of the first occurrence of `needle` in a file.
    pub fn at(&self, name: &str, needle: &str) -> u32 {
        let file = self.file(name);
        let text = self.db.text(file).unwrap();
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found in {name}")) as u32
    }

    /// True when a JDK standard library was found, so tests that need it can
    /// report a skip rather than fail on a machine without one.
    pub fn has_stdlib(&self) -> bool {
        self.db.classpath().contains("java.util.ArrayList")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

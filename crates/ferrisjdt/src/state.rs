//! Server state: the database, the pending index queue, and dirty tracking.

use java_ide::completion::CompletionSettings;
use java_semantic::symbols::FileId;
use java_semantic::Db;
use java_syntax::SourceFile;
use project_model::{Config, Project};
use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Files indexed per slice of background work.
///
/// Indexing runs on the main loop between messages rather than on a worker
/// thread, which keeps the database single-owner and free of locks. The slice
/// is small enough that an incoming request waits microseconds, not seconds.
const INDEX_SLICE: usize = 8;

pub struct State {
    pub db: Db,
    /// Source files still to be indexed, most recent last.
    pending: Vec<PathBuf>,
    /// Files whose diagnostics need republishing.
    dirty: FxHashSet<FileId>,
    /// Requests the client has cancelled. Keyed by the protocol's own id type,
    /// which may be a number or a string.
    cancelled: FxHashSet<lsp_server::RequestId>,
    pub completion_settings: CompletionSettings,
    pub supports_location_links: bool,
    indexing_started: Option<Instant>,
    pub indexed_files: usize,
}

impl State {
    pub fn new(root: &Path, config: Config) -> State {
        let project = Project::load(root, &config);
        let pending = project.discover_source_files();
        tracing::info!(
            root = %root.display(),
            files = pending.len(),
            source_level = %project.source_level,
            "loaded project"
        );
        State {
            db: Db::new(project),
            pending,
            dirty: FxHashSet::default(),
            cancelled: FxHashSet::default(),
            completion_settings: CompletionSettings::default(),
            supports_location_links: false,
            indexing_started: None,
            indexed_files: 0,
        }
    }

    pub fn is_indexing(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Indexes the next slice of files. Returns true while work remains.
    pub fn index_slice(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        if self.indexing_started.is_none() {
            self.indexing_started = Some(Instant::now());
        }
        for _ in 0..INDEX_SLICE {
            let Some(path) = self.pending.pop() else {
                break;
            };
            // Parse once, index the outline, and drop the tree: outlines are
            // what other files need, and a tree can be rebuilt on demand.
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = SourceFile::new(text, 0);
            self.db.index_parsed(&path, parsed);
            self.indexed_files += 1;
        }
        if self.pending.is_empty() {
            if let Some(started) = self.indexing_started.take() {
                let stats = self.db.cache_stats();
                tracing::info!(
                    files = self.indexed_files,
                    classes = stats.workspace_classes,
                    elapsed_ms = started.elapsed().as_millis(),
                    "workspace index complete"
                );
            }
            return false;
        }
        true
    }

    pub fn mark_dirty(&mut self, file: FileId) {
        self.dirty.insert(file);
    }

    pub fn take_dirty(&mut self) -> Vec<FileId> {
        let mut files: Vec<FileId> = self.dirty.drain().collect();
        files.sort();
        files
    }

    pub fn cancel(&mut self, id: lsp_server::RequestId) {
        self.cancelled.insert(id);
    }

    /// Whether a request was cancelled, consuming the record.
    pub fn is_cancelled(&mut self, id: &lsp_server::RequestId) -> bool {
        self.cancelled.remove(id)
    }

    /// Resolves a document URI to an indexed file, indexing it if the client
    /// opened something outside the discovered source roots.
    pub fn file_for(&mut self, uri: &lsp_types::Uri) -> Option<FileId> {
        let path = java_ide::uri::to_path(uri)?;
        if let Some(file) = self.db.file_id(&path) {
            return Some(file);
        }
        self.db.load_from_disk(&path)
    }
}

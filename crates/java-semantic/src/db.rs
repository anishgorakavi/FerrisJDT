//! The analysis database.
//!
//! Incremental strategy, in one place so it can be reasoned about:
//!
//! * Each file keeps its text, its version, and its *outline* — package,
//!   imports, and declared class names. Outlines are what other files can
//!   observe about this one.
//! * `outline_revision` increases only when some file's outline changes shape.
//!   Typing inside a method body does not change it, so nothing outside the
//!   edited file is invalidated.
//! * Lowered `ClassData` is cached per file and keyed on both that file's
//!   version and `outline_revision`, so it is recomputed exactly when one of
//!   its inputs changed.
//! * Parse trees are kept for open documents and in a small cache for the
//!   rest, because a closed file's tree can be rebuilt from its text in about
//!   a millisecond and holding every tree would dominate memory.
//! * Each file also keeps its last parse that still produced all of its
//!   declarations. Error recovery occasionally collapses a whole class — a
//!   bare `this.` is enough — and without this the class would vanish from
//!   the index mid-keystroke, taking completion and every cross-file
//!   reference to it along. Resolution reads the retained parse; anything
//!   that produces ranges in the live document reads the current one.

use crate::classpath::Classpath;
use crate::lower;
use crate::name::{self, Name};
use crate::outline::{self, FileOutline};
use crate::symbols::{ClassRef, FileId};
use java_syntax::SourceFile;
use project_model::Project;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Parse trees kept for files that are not open in the editor.
const TREE_CACHE_CAPACITY: usize = 48;
/// Files whose lowered declarations are kept.
const LOWERED_CACHE_CAPACITY: usize = 512;

struct FileState {
    path: PathBuf,
    text: Arc<str>,
    version: i32,
    /// True while the editor owns the contents.
    is_open: bool,
    outline: Arc<FileOutline>,
    /// Kept only for open documents, which are edited and queried constantly.
    open_source: Option<Arc<SourceFile>>,
    /// The text and outline of the most recent parse that yielded every
    /// declaration in the file, used for resolution while the live text is
    /// momentarily unparseable.
    ///
    /// The *text* is retained rather than the parse tree. In the ordinary case
    /// it is the very same `Arc` as `text`, so this costs nothing; a tree
    /// would cost many times the size of the source for every indexed file.
    retained: Option<(Arc<str>, Arc<FileOutline>)>,
}

/// Parse trees for files the editor has not opened, evicted oldest first.
#[derive(Default)]
struct TreeCache {
    trees: FxHashMap<FileId, Arc<SourceFile>>,
    /// Insertion order, for eviction.
    order: Vec<FileId>,
}

impl TreeCache {
    fn get(&self, file: FileId) -> Option<Arc<SourceFile>> {
        self.trees.get(&file).cloned()
    }

    fn insert(&mut self, file: FileId, source: Arc<SourceFile>) {
        if self.order.len() >= TREE_CACHE_CAPACITY {
            if let Some(evicted) = self.order.first().copied() {
                self.order.remove(0);
                self.trees.remove(&evicted);
            }
        }
        if self.trees.insert(file, source).is_none() {
            self.order.push(file);
        }
    }

    fn remove(&mut self, file: FileId) {
        self.trees.remove(&file);
        self.order.retain(|held| *held != file);
    }

    fn clear(&mut self) {
        self.trees.clear();
        self.order.clear();
    }

    fn len(&self) -> usize {
        self.trees.len()
    }
}

struct LoweredEntry {
    outline_revision: u64,
    version: i32,
    classes: Arc<Vec<ClassRef>>,
}

/// Maps class names declared in the workspace to the file that declares them.
#[derive(Default)]
struct SourceIndex {
    by_binary: FxHashMap<Name, FileId>,
    by_simple: FxHashMap<Name, Vec<Name>>,
}

impl SourceIndex {
    fn insert(&mut self, outline: &FileOutline) {
        let generated = outline
            .synthetic_classes()
            .into_iter()
            .map(|(binary, simple, _)| (binary, simple));
        let declared = outline
            .classes
            .iter()
            .map(|c| (c.binary.clone(), c.simple.clone()));
        for (binary, simple) in declared.chain(generated) {
            self.by_binary.insert(binary.clone(), outline.file);
            let bucket = self.by_simple.entry(simple).or_default();
            if !bucket.contains(&binary) {
                bucket.push(binary);
            }
        }
    }

    fn remove(&mut self, outline: &FileOutline) {
        let generated = outline
            .synthetic_classes()
            .into_iter()
            .map(|(binary, simple, _)| (binary, simple));
        let declared = outline
            .classes
            .iter()
            .map(|c| (c.binary.clone(), c.simple.clone()));
        for (binary, simple) in declared.chain(generated) {
            if self.by_binary.get(&binary) == Some(&outline.file) {
                self.by_binary.remove(&binary);
            }
            if let Some(bucket) = self.by_simple.get_mut(&simple) {
                bucket.retain(|b| *b != binary);
                if bucket.is_empty() {
                    self.by_simple.remove(&simple);
                }
            }
        }
    }
}

pub struct Db {
    project: Project,
    classpath: Classpath,
    files: Vec<FileState>,
    by_path: FxHashMap<PathBuf, FileId>,
    index: SourceIndex,
    outline_revision: u64,
    trees: RefCell<TreeCache>,
    lowered: RefCell<FxHashMap<FileId, LoweredEntry>>,
    /// Files currently being lowered, to stop a cyclic declaration from
    /// recursing without bound.
    lowering: RefCell<FxHashSet<FileId>>,
}

impl Db {
    pub fn new(project: Project) -> Db {
        let classpath = Classpath::load(&project);
        Db {
            project,
            classpath,
            files: Vec::new(),
            by_path: FxHashMap::default(),
            index: SourceIndex::default(),
            outline_revision: 1,
            trees: RefCell::new(TreeCache::default()),
            lowered: RefCell::new(FxHashMap::default()),
            lowering: RefCell::new(FxHashSet::default()),
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn classpath(&self) -> &Classpath {
        &self.classpath
    }

    pub fn outline_revision(&self) -> u64 {
        self.outline_revision
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn open_file_count(&self) -> usize {
        self.files.iter().filter(|f| f.is_open).count()
    }

    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    pub fn path(&self, file: FileId) -> Option<&Path> {
        self.files.get(file.0 as usize).map(|f| f.path.as_path())
    }

    pub fn text(&self, file: FileId) -> Option<Arc<str>> {
        self.files.get(file.0 as usize).map(|f| f.text.clone())
    }

    pub fn version(&self, file: FileId) -> Option<i32> {
        self.files.get(file.0 as usize).map(|f| f.version)
    }

    pub fn is_open(&self, file: FileId) -> bool {
        self.files
            .get(file.0 as usize)
            .map(|f| f.is_open)
            .unwrap_or(false)
    }

    /// The outline of the file as it currently reads. Anything that produces
    /// ranges in the live document must use this.
    pub fn outline(&self, file: FileId) -> Option<Arc<FileOutline>> {
        self.files.get(file.0 as usize).map(|f| f.outline.clone())
    }

    /// The parse used for resolution: the current one, or the last that still
    /// had every declaration. Name lookup and lowering go through here.
    pub fn resolution_source(&self, file: FileId) -> Option<(Arc<SourceFile>, Arc<FileOutline>)> {
        let state = self.files.get(file.0 as usize)?;
        match &state.retained {
            // The usual case: the retained text is the current text, so the
            // cached tree serves.
            Some((text, outline)) if Arc::ptr_eq(text, &state.text) => {
                Some((self.source(file)?, outline.clone()))
            }
            // The live text no longer parses completely. Re-parse the text
            // that did, so the outline's offsets match the tree. This happens
            // only for a file being typed in, so it is not worth caching.
            Some((text, outline)) => Some((
                Arc::new(SourceFile::new(text.to_string(), state.version)),
                outline.clone(),
            )),
            None => Some((self.source(file)?, state.outline.clone())),
        }
    }

    /// The outline resolution uses, which may predate the current text.
    pub fn resolution_outline(&self, file: FileId) -> Option<Arc<FileOutline>> {
        self.resolution_source(file).map(|(_, outline)| outline)
    }

    pub fn all_files(&self) -> impl Iterator<Item = FileId> + '_ {
        (0..self.files.len() as u32).map(FileId)
    }

    /// The parse tree for a file, parsing it if necessary.
    pub fn source(&self, file: FileId) -> Option<Arc<SourceFile>> {
        let state = self.files.get(file.0 as usize)?;
        if let Some(source) = &state.open_source {
            return Some(source.clone());
        }
        if let Some(cached) = self.trees.borrow().get(file) {
            return Some(cached);
        }
        let parsed = Arc::new(SourceFile::new(state.text.to_string(), state.version));
        self.trees.borrow_mut().insert(file, parsed.clone());
        Some(parsed)
    }

    fn intern_path(&mut self, path: &Path) -> FileId {
        if let Some(existing) = self.by_path.get(path) {
            return *existing;
        }
        let file = FileId(self.files.len() as u32);
        self.files.push(FileState {
            path: path.to_path_buf(),
            text: Arc::from(""),
            version: 0,
            is_open: false,
            outline: Arc::new(FileOutline {
                file,
                package: None,
                imports: Vec::new(),
                classes: Vec::new(),
                lombok: false,
            }),
            open_source: None,
            retained: None,
        });
        self.by_path.insert(path.to_path_buf(), file);
        file
    }

    /// Records a file's contents, updating the index and, when its outline
    /// changed shape, the global revision.
    fn set_contents(&mut self, file: FileId, source: Arc<SourceFile>, keep_tree: bool) {
        let new_outline = Arc::new(outline::outline(file, &source));
        let state = &self.files[file.0 as usize];
        let retained = state.retained.clone();
        let previous_indexed = retained
            .as_ref()
            .map(|(_, outline)| outline.clone())
            .unwrap_or_else(|| state.outline.clone());

        // A parse that lost declarations the file still has is error recovery
        // mid-edit, not an edit that removed them. The previous parse stays
        // authoritative for resolution until a complete one arrives.
        let lost_declarations = source.tree().root_node().has_error()
            && new_outline.classes.len() < previous_indexed.classes.len();
        let indexed = if lost_declarations {
            tracing::debug!(
                file = ?file,
                "parse lost declarations; keeping the previous one for resolution"
            );
            previous_indexed.clone()
        } else {
            new_outline.clone()
        };

        if !previous_indexed.same_shape(&indexed) {
            self.index.remove(&previous_indexed);
            self.index.insert(&indexed);
            self.outline_revision += 1;
        }

        let state = &mut self.files[file.0 as usize];
        state.text = Arc::from(source.text());
        state.version = source.version();
        state.outline = new_outline;
        state.retained = if lost_declarations {
            retained
        } else {
            Some((state.text.clone(), indexed))
        };
        if keep_tree {
            state.open_source = Some(source);
        } else {
            state.open_source = None;
            self.trees.borrow_mut().insert(file, source);
        }
        // The file's own lowered data always depends on its text.
        self.lowered.borrow_mut().remove(&file);
    }

    /// Adds or refreshes a file from disk. Open documents are left alone,
    /// since the editor's copy is authoritative.
    pub fn load_from_disk(&mut self, path: &Path) -> Option<FileId> {
        let text = std::fs::read_to_string(path).ok()?;
        let file = self.intern_path(path);
        if self.files[file.0 as usize].is_open {
            return Some(file);
        }
        let source = Arc::new(SourceFile::new(text, 0));
        self.set_contents(file, source, false);
        // A freshly indexed file's tree is not worth keeping; its outline is.
        self.trees.borrow_mut().remove(file);
        Some(file)
    }

    /// Indexes a file given text already in hand, which is how background
    /// indexing hands its work back without re-reading the file.
    pub fn index_parsed(&mut self, path: &Path, source: SourceFile) -> FileId {
        let file = self.intern_path(path);
        if self.files[file.0 as usize].is_open {
            return file;
        }
        self.set_contents(file, Arc::new(source), false);
        self.trees.borrow_mut().remove(file);
        file
    }

    pub fn open_document(&mut self, path: &Path, text: String, version: i32) -> FileId {
        let file = self.intern_path(path);
        let source = Arc::new(SourceFile::new(text, version));
        self.files[file.0 as usize].is_open = true;
        self.set_contents(file, source, true);
        self.trees.borrow_mut().remove(file);
        file
    }

    /// Applies an incremental change to an open document.
    ///
    /// `edit` receives the document and performs the edit; it is given `&mut`
    /// access so a sequence of changes in one notification shares one reparse
    /// per change without cloning the tree.
    pub fn edit_document(
        &mut self,
        file: FileId,
        version: i32,
        edit: impl FnOnce(&mut SourceFile),
    ) -> bool {
        let Some(state) = self.files.get_mut(file.0 as usize) else {
            return false;
        };
        let is_open = state.is_open;
        let mut source = match state.open_source.take() {
            Some(source) => match Arc::try_unwrap(source) {
                Ok(owned) => owned,
                // A request holds a clone; rebuild from text rather than block.
                Err(shared) => SourceFile::new(shared.text().to_string(), shared.version()),
            },
            // A file the editor has not opened can still be edited, which is
            // what speculative analysis of a closed file needs.
            None => match self.source(file) {
                Some(parsed) => SourceFile::new(parsed.text().to_string(), parsed.version()),
                None => return false,
            },
        };
        edit(&mut source);
        let _ = version;
        self.set_contents(file, Arc::new(source), is_open);
        true
    }

    pub fn close_document(&mut self, file: FileId) {
        if let Some(state) = self.files.get_mut(file.0 as usize) {
            state.is_open = false;
            let source = state.open_source.take();
            // Fall back to what is on disk, which may differ from the buffer.
            if let Ok(text) = std::fs::read_to_string(&state.path) {
                if &*state.text != text.as_str() {
                    let parsed = Arc::new(SourceFile::new(text, 0));
                    self.set_contents(file, parsed, false);
                    self.trees.borrow_mut().remove(file);
                    return;
                }
            }
            if let Some(source) = source {
                self.trees.borrow_mut().insert(file, source);
            }
        }
    }

    /// Forgets a deleted file.
    pub fn remove_file(&mut self, file: FileId) {
        let Some(state) = self.files.get_mut(file.0 as usize) else {
            return;
        };
        let outline = state
            .retained
            .as_ref()
            .map(|(_, outline)| outline.clone())
            .unwrap_or_else(|| state.outline.clone());
        state.is_open = false;
        state.text = Arc::from("");
        state.open_source = None;
        state.retained = None;
        state.outline = Arc::new(FileOutline {
            file,
            package: None,
            imports: Vec::new(),
            classes: Vec::new(),
            lombok: false,
        });
        if !outline.classes.is_empty() {
            self.index.remove(&outline);
            self.outline_revision += 1;
        }
        self.trees.borrow_mut().remove(file);
        self.lowered.borrow_mut().remove(&file);
    }

    /// True when a binary class name is declared in the workspace or exists on
    /// the classpath. Deliberately cheap: it never decodes or lowers anything.
    pub fn has_class(&self, binary: &str) -> bool {
        self.index.by_binary.contains_key(binary) || self.classpath.contains(binary)
    }

    /// Looks up a class by binary name, lowering or decoding on demand.
    pub fn class(&self, binary: &str) -> Option<ClassRef> {
        if let Some(&file) = self.index.by_binary.get(binary) {
            let classes = self.lowered_classes(file);
            if let Some(found) = classes.iter().find(|c| &*c.name == binary) {
                return Some(found.clone());
            }
        }
        self.classpath.class(binary)
    }

    /// A class as declared in `file` when that file declares it, otherwise as the project knows it.
    ///
    /// Several modules of a build can declare the same qualified name (test classes especially), and
    /// the project-wide index keeps only one of them. A file always means its own.
    pub fn class_declared_in(&self, file: FileId, binary: &str) -> Option<ClassRef> {
        let own = self.lowered_classes(file);
        match own.iter().find(|class| &*class.name == binary) {
            Some(found) => Some(found.clone()),
            None => self.class(binary),
        }
    }

    /// Every class declared in one file, lowered and cached.
    pub fn lowered_classes(&self, file: FileId) -> Arc<Vec<ClassRef>> {
        let Some(state) = self.files.get(file.0 as usize) else {
            return Arc::new(Vec::new());
        };
        if let Some(entry) = self.lowered.borrow().get(&file) {
            if entry.outline_revision == self.outline_revision && entry.version == state.version {
                return entry.classes.clone();
            }
        }
        // A cyclic declaration would otherwise re-enter here while this file is
        // still being lowered; returning nothing is conservative and finite.
        if !self.lowering.borrow_mut().insert(file) {
            return Arc::new(Vec::new());
        }
        let result = match self.resolution_source(file) {
            Some((source, outline)) => Arc::new(lower::lower_file(self, &source, &outline)),
            None => Arc::new(Vec::new()),
        };
        self.lowering.borrow_mut().remove(&file);

        let mut cache = self.lowered.borrow_mut();
        if cache.len() >= LOWERED_CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(
            file,
            LoweredEntry {
                outline_revision: self.outline_revision,
                version: state.version,
                classes: result.clone(),
            },
        );
        result
    }

    /// Binary names of workspace and classpath classes with a given simple name.
    pub fn classes_named(&self, simple: &str) -> Vec<Name> {
        let mut names: Vec<Name> = self
            .index
            .by_simple
            .get(simple)
            .cloned()
            .unwrap_or_default();
        for binary in self.classpath.classes_named(simple) {
            names.push(name::name(binary));
        }
        names
    }

    /// Binary names declared directly in a package.
    pub fn classes_in_package(&self, package: &str) -> Vec<Name> {
        let mut names: Vec<Name> = self
            .index
            .by_binary
            .keys()
            .filter(|binary| {
                name::package_of(binary) == package && name::enclosing_class(binary).is_none()
            })
            .cloned()
            .collect();
        for binary in self.classpath.classes_in_package(package) {
            if name::enclosing_class(binary).is_none() {
                names.push(name::name(binary));
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Immediate subpackages of a package, for completing a qualified name.
    pub fn subpackages(&self, package: &str) -> Vec<Name> {
        let skip = package.len() + usize::from(!package.is_empty());
        let mut names: Vec<Name> = self
            .classpath
            .subpackages(package)
            .into_iter()
            .map(name::name)
            .collect();
        for binary in self.index.by_binary.keys() {
            let Some(rest) = binary.strip_prefix(package) else {
                continue;
            };
            let rest = if package.is_empty() { &**binary } else { rest };
            if !package.is_empty() && !rest.starts_with('.') {
                continue;
            }
            let rest = &binary[skip.min(binary.len())..];
            if let Some(head) = rest.split('.').next() {
                if rest.len() > head.len() {
                    names.push(name::name(head));
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// True when the name denotes a package rather than a class.
    pub fn is_package(&self, candidate: &str) -> bool {
        if candidate.is_empty() {
            return false;
        }
        !self.classes_in_package(candidate).is_empty() || !self.subpackages(candidate).is_empty()
    }

    /// All class names declared in the workspace, for workspace symbols.
    pub fn workspace_class_names(&self) -> impl Iterator<Item = (&Name, FileId)> {
        self.index
            .by_binary
            .iter()
            .map(|(name, file)| (name, *file))
    }

    /// Clears derived caches without discarding texts or outlines. Used when
    /// the classpath or configuration changes.
    pub fn invalidate_derived(&mut self) {
        self.lowered.borrow_mut().clear();
        self.trees.borrow_mut().clear();
        self.outline_revision += 1;
    }

    /// Replaces the classpath, for example after the JDK setting changes.
    pub fn set_classpath(&mut self, classpath: Classpath) {
        self.classpath = classpath;
        self.invalidate_derived();
    }

    /// Counts held for the `ferrisjdt/status` request and for benchmarks.
    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            files: self.files.len(),
            open_files: self.open_file_count(),
            workspace_classes: self.index.by_binary.len(),
            classpath_classes: self.classpath.class_count(),
            cached_trees: self.trees.borrow().len(),
            cached_lowered_files: self.lowered.borrow().len(),
            cached_classpath_classes: self.classpath.cached_classes(),
            outline_revision: self.outline_revision,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub files: usize,
    pub open_files: usize,
    pub workspace_classes: usize,
    pub classpath_classes: usize,
    pub cached_trees: usize,
    pub cached_lowered_files: usize,
    pub cached_classpath_classes: usize,
    pub outline_revision: u64,
}

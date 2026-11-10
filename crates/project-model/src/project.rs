//! Source roots, the classpath, and the bounded source file walk.

use crate::build_files;
use crate::config::{Config, SourceLevel};
use crate::jdk::{self, Jdk};
use std::path::{Path, PathBuf};

/// Directory names that never contain project sources worth indexing.
const DEFAULT_EXCLUDES: &[&str] = &[
    "target",
    "build",
    "out",
    "bin",
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    ".idea",
    ".gradle",
    ".settings",
    ".metadata",
    "__pycache__",
    ".venv",
];

/// Default excludes that are also legal package names (`org.gradle.internal.build`). They are
/// skipped when scanning a project root for stray sources, but not inside a source root, where a
/// directory is a package.
const PACKAGE_LIKE_EXCLUDES: &[&str] = &["target", "build", "out", "bin"];

/// Guards against walking a pathological tree. Both are deliberately generous
/// for a course project yet bounded, as untrusted input requires.
const MAX_DEPTH: usize = 24;
const DEFAULT_MAX_SOURCE_FILES: usize = 20_000;

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub source_level: SourceLevel,
    pub jdk: Option<Jdk>,
    /// JARs and class directories, excluding the standard library.
    pub classpath: Vec<PathBuf>,
    /// Directories to search for sources. Derived roots are added by the index
    /// once package declarations are known.
    pub source_roots: Vec<PathBuf>,
    /// Directories a client listed in `.ferrisjdt/sources.txt`. Each is walked from its own top, so
    /// generated sources under an excluded directory name such as `build` are still indexed.
    pub listed_source_roots: Vec<PathBuf>,
    /// Files the build excludes from compilation, from `.ferrisjdt/excluded.txt`.
    pub excluded_files: Vec<PathBuf>,
    pub max_source_files: usize,
    excludes: Vec<String>,
    /// `excludes` without the package-like defaults, used inside source roots.
    source_root_excludes: Vec<String>,
}

impl Project {
    /// Builds a project description from a root directory and client settings.
    /// Only the filesystem is consulted; no build tool is invoked. An explicit
    /// source level wins over the one a `pom.xml` or `build.gradle` declares.
    pub fn load(opened: &Path, config: &Config) -> Project {
        // Opening one module of a multi-module build makes the parent project the workspace.
        let root = build_files::enclosing_build_root(opened);
        let root = root.as_path();
        let source_level = SourceLevel(
            config
                .source_level
                .or_else(|| build_files::read_level_file(opened))
                .or_else(|| build_files::read_level_file(root))
                .or_else(|| build_files::detect_source_level(opened))
                .or_else(|| build_files::detect_source_level(root))
                .unwrap_or(8),
        );
        let jdk = jdk::discover(config.jdk_home.as_deref(), source_level.0);
        if let Some(jdk) = &jdk {
            tracing::info!(
                home = %jdk.home.display(),
                stdlib = %jdk.stdlib.path().display(),
                "found jdk"
            );
        } else {
            tracing::warn!("no jdk found; standard library completion is unavailable");
        }

        let mut excludes: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        excludes.extend(config.exclude.iter().cloned());
        let source_root_excludes: Vec<String> = excludes
            .iter()
            .filter(|name| {
                !PACKAGE_LIKE_EXCLUDES.contains(&name.as_str()) || config.exclude.contains(name)
            })
            .cloned()
            .collect();

        let mut source_roots = Vec::new();
        let push_root = |path: PathBuf, roots: &mut Vec<PathBuf>| {
            if path.is_dir() && !roots.contains(&path) {
                roots.push(path);
            }
        };
        for candidate in ["src/main/java", "src/test/java", "src", "test", "tests"] {
            push_root(root.join(candidate), &mut source_roots);
        }
        for extra in &config.source_roots {
            let path = if extra.is_absolute() {
                extra.clone()
            } else {
                root.join(extra)
            };
            push_root(path, &mut source_roots);
        }
        let mut listed_source_roots = build_files::read_sources_file(root);
        if opened != root {
            listed_source_roots.extend(build_files::read_sources_file(opened));
        }
        // A flat directory of .java files is the common course-project layout. A build tool's own
        // list of source directories says where the code is, so the whole tree is not scanned too.
        if source_roots.is_empty() && listed_source_roots.is_empty() {
            source_roots.push(root.to_path_buf());
        }

        let mut classpath = Vec::new();
        for dir in ["lib", "libs", "jars"] {
            collect_jars(&root.join(dir), &mut classpath);
        }
        // Resolved by the client from pom.xml or build.gradle; see `build_files`.
        classpath.extend(build_files::read_classpath_file(root));
        if opened != root {
            for jar in build_files::read_classpath_file(opened) {
                if !classpath.contains(&jar) {
                    classpath.push(jar);
                }
            }
        }
        for extra in &config.classpath {
            let path = if extra.is_absolute() {
                extra.clone()
            } else {
                root.join(extra)
            };
            if path.is_dir() {
                collect_jars(&path, &mut classpath);
            } else if path.is_file() {
                classpath.push(path);
            }
        }

        let mut excluded_files = build_files::read_excluded_file(root);
        if opened != root {
            excluded_files.extend(build_files::read_excluded_file(opened));
        }

        Project {
            root: root.to_path_buf(),
            source_level,
            jdk,
            classpath,
            source_roots,
            listed_source_roots,
            excluded_files,
            max_source_files: config.max_source_files.unwrap_or(DEFAULT_MAX_SOURCE_FILES),
            excludes,
            source_root_excludes,
        }
    }

    /// Lists the `.java` files to index.
    ///
    /// The walk refuses to follow symlinks, so a link pointing outside the
    /// project cannot pull unrelated trees into the index, and it stops at a
    /// fixed depth and file count.
    pub fn discover_source_files(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut roots = self.source_roots.clone();
        // Nested roots would yield duplicates; keep only the outermost.
        roots.sort();
        roots.dedup_by(|inner, outer| inner.starts_with(&*outer));
        for root in roots.iter().chain(&self.listed_source_roots) {
            // Below a real source root every directory is a package; only a project root that
            // doubles as the source root (the flat course layout) is filtered by name.
            self.walk(root, 0, *root != self.root, &mut found);
        }
        found.sort();
        found.dedup();
        if !self.excluded_files.is_empty() {
            found.retain(|file| !self.excluded_files.contains(file));
        }
        found
    }

    fn walk(&self, dir: &Path, depth: usize, in_source_root: bool, found: &mut Vec<PathBuf>) {
        if depth > MAX_DEPTH || found.len() >= self.max_source_files {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut subdirectories = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // `file_type` from `read_dir` does not follow links, so this
            // rejects symlinked files and directories alike.
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                let excludes = if in_source_root {
                    &self.source_root_excludes
                } else {
                    &self.excludes
                };
                if name.starts_with('.') || excludes.iter().any(|e| *e == name) {
                    continue;
                }
                subdirectories.push(entry.path());
            } else if name.ends_with(".java") && !name.starts_with('.') {
                if found.len() >= self.max_source_files {
                    tracing::warn!(limit = self.max_source_files, "source file limit reached");
                    return;
                }
                found.push(entry.path());
            }
        }
        subdirectories.sort();
        for subdirectory in subdirectories {
            self.walk(&subdirectory, depth + 1, in_source_root, found);
        }
    }

    /// True when `path` lies inside the project root, used to refuse requests
    /// for files outside the workspace.
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut jars: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "jar").unwrap_or(false))
        .collect();
    jars.sort();
    out.extend(jars);
}

/// Reads a `ferrisjdt.json` from the project root, if present. A malformed file
/// is reported and ignored rather than failing startup.
pub fn load_config_file(root: &Path) -> Config {
    let path = root.join("ferrisjdt.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    match serde_json::from_str(&text) {
        Ok(config) => {
            tracing::info!(path = %path.display(), "loaded project config");
            config
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring malformed project config");
            Config::default()
        }
    }
}

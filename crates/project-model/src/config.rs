//! Client and project settings, and the Java language level.

use serde::Deserialize;
use std::path::PathBuf;

/// Settings a client may supply through `initializationOptions` or a
/// `ferrisjdt.json` file at the project root. Everything is optional; the
/// defaults are what a plain Java source tree needs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// JDK to read the standard library from. Discovered when absent.
    pub jdk_home: Option<PathBuf>,
    /// Source level, as a number: 8 for Java 8.
    pub source_level: Option<u32>,
    /// Extra source roots, relative to the project root or absolute.
    pub source_roots: Vec<PathBuf>,
    /// Extra JARs or class directories to index.
    pub classpath: Vec<PathBuf>,
    /// Directory names never descended into, in addition to the defaults.
    pub exclude: Vec<String>,
    /// Upper bound on indexed source files, as a guard on pathological trees.
    pub max_source_files: Option<usize>,
}

/// The Java language level the server analyses against.
///
/// Only the distinction that changes observable behaviour is modelled: which
/// APIs and language constructs are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceLevel(pub u32);

impl SourceLevel {
    pub const JAVA_8: SourceLevel = SourceLevel(8);

    pub fn allows_lambdas(self) -> bool {
        self.0 >= 8
    }

    /// `var` for local variables arrived in Java 10.
    pub fn allows_var(self) -> bool {
        self.0 >= 10
    }

    /// `record`, `sealed`, and switch expressions are all past Java 8.
    pub fn allows_records(self) -> bool {
        self.0 >= 16
    }

    pub fn allows_switch_expressions(self) -> bool {
        self.0 >= 14
    }

    pub fn allows_text_blocks(self) -> bool {
        self.0 >= 15
    }
}

impl Default for SourceLevel {
    fn default() -> Self {
        SourceLevel::JAVA_8
    }
}

impl std::fmt::Display for SourceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

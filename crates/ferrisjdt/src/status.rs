//! A small server-specific request reporting index and cache state.
//!
//! This is an extension, not something a client needs: every standard feature
//! works without it. It exists so the benchmarks and the `--self-check` mode
//! can read the same counters a running server would report.

use crate::state::State;
use serde::Serialize;

pub const METHOD: &str = "ferrisjdt/status";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub version: &'static str,
    pub indexing: bool,
    pub pending_files: usize,
    pub source_level: String,
    pub jdk_home: Option<String>,
    pub stdlib: Option<String>,
    #[serde(flatten)]
    pub caches: java_semantic::CacheStats,
}

pub fn status(state: &State) -> Status {
    let project = state.db.project();
    Status {
        version: env!("CARGO_PKG_VERSION"),
        indexing: state.is_indexing(),
        pending_files: state.pending_count(),
        source_level: project.source_level.to_string(),
        jdk_home: project
            .jdk
            .as_ref()
            .map(|jdk| jdk.home.display().to_string()),
        stdlib: project
            .jdk
            .as_ref()
            .map(|jdk| jdk.stdlib.path().display().to_string()),
        caches: state.db.cache_stats(),
    }
}

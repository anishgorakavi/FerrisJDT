//! Runs the whole analysis over a large body of real Java and asserts only
//! that nothing crashes, hangs, or reports an impossible range.
//!
//! The corpus is the Eclipse JDT language server's own source tree when it is
//! checked out beside this repository, or whatever `FERRISJDT_CORPUS` points
//! at. Its third-party dependencies are not on the classpath, so it is not a
//! correctness corpus — unresolved types there are expected and correct. What
//! it does test is that a thousand files of unfamiliar, generics-heavy,
//! annotation-heavy Java cannot take the server down.

use java_ide::{completion, diagnostics, hover, navigation, symbols, tokens};
use java_semantic::Db;
use java_syntax::SourceFile;
use project_model::{Config, Project};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn corpus() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("FERRISJDT_CORPUS") {
        let path = PathBuf::from(path);
        return path.is_dir().then_some(path);
    }
    let beside = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../eclipse.jdt.ls")
        .canonicalize()
        .ok()?;
    beside.is_dir().then_some(beside)
}

#[test]
fn analyses_a_large_real_corpus_without_crashing() {
    let Some(root) = corpus() else {
        eprintln!("skipping: no corpus; set FERRISJDT_CORPUS to a Java source tree");
        return;
    };
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    assert!(
        files.len() > 100,
        "expected a large corpus, found {}",
        files.len()
    );

    let mut db = Db::new(project);
    let indexing = Instant::now();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        db.index_parsed(path, SourceFile::new(text, 0));
    }
    let index_time = indexing.elapsed();
    assert!(
        index_time < Duration::from_secs(60),
        "indexing {} files took {index_time:?}",
        files.len()
    );

    // Every file: diagnostics, symbols, and tokens, with sane ranges.
    let mut slowest = (Duration::ZERO, PathBuf::new());
    for file in db.all_files() {
        let Some(text) = db.text(file) else { continue };
        let lines = text.lines().count() as u32;
        let started = Instant::now();

        let found = diagnostics::diagnostics(&db, file);
        for item in &found {
            assert!(
                item.range.start.line <= lines && item.range.end.line <= lines,
                "diagnostic outside the document in {:?}: {:?}",
                db.path(file),
                item.range
            );
            assert!(
                (item.range.start.line, item.range.start.character)
                    <= (item.range.end.line, item.range.end.character),
                "inverted diagnostic range in {:?}",
                db.path(file)
            );
        }

        for symbol in symbols::document_symbols(&db, file) {
            assert!(
                symbol.range.end.line <= lines,
                "symbol outside {:?}",
                db.path(file)
            );
        }
        if let Some(found) = tokens::semantic_tokens(&db, file) {
            for token in &found.data {
                assert!(token.length > 0);
            }
        }

        let elapsed = started.elapsed();
        if elapsed > slowest.0 {
            slowest = (elapsed, db.path(file).unwrap_or(&root).to_path_buf());
        }
    }
    eprintln!(
        "corpus: {} files indexed in {:?}; slowest file {:?} at {:?}",
        files.len(),
        index_time,
        slowest.1.file_name(),
        slowest.0
    );
    // A single file must stay well inside interactive range.
    assert!(
        slowest.0 < Duration::from_secs(2),
        "{:?} took {:?} to analyse",
        slowest.1,
        slowest.0
    );
}

#[test]
fn requests_at_every_identifier_in_a_sample_never_panic() {
    let Some(root) = corpus() else { return };
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    let mut db = Db::new(project);
    // A spread of files rather than all of them: this test is about surviving
    // a request at an arbitrary position, not about coverage of the corpus.
    let sample: Vec<_> = files.iter().step_by(37).take(20).cloned().collect();
    for path in &sample {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        db.index_parsed(path, SourceFile::new(text, 0));
    }
    for path in &sample {
        let Some(file) = db.file_id(path) else {
            continue;
        };
        let Some(text) = db.text(file) else { continue };
        // Every 101st byte, which lands inside and between tokens alike.
        for offset in (0..text.len() as u32).step_by(101) {
            let _ = hover::hover(&db, file, offset);
            let _ = navigation::definition(&db, file, offset);
            let _ = navigation::document_highlights(&db, file, offset);
            let _ = completion::completions(&db, file, offset, Default::default());
        }
        // And at the very end of the file, a position editors do send.
        let end = text.len() as u32;
        let _ = hover::hover(&db, file, end);
        let _ = completion::completions(&db, file, end, Default::default());
    }
}

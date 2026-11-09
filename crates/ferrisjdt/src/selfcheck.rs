//! `--self-check`: index a project and report what was found, with timings.
//!
//! This exercises the same code paths as the server without needing an editor,
//! which is what makes the benchmark numbers reproducible and is the quickest
//! way to see whether a real project resolves.

use java_semantic::Db;
use java_syntax::SourceFile;
use project_model::Project;
use std::path::Path;
use std::time::Instant;

pub fn self_check(root: &Path, verbose: bool) -> i32 {
    let total = Instant::now();
    let config = project_model::load_config_file(root);
    let load = Instant::now();
    let project = Project::load(root, &config);
    let project_ms = load.elapsed().as_secs_f64() * 1000.0;

    println!("project root:   {}", root.display());
    println!("source level:   {}", project.source_level);
    match &project.jdk {
        Some(jdk) => {
            println!("jdk home:       {}", jdk.home.display());
            println!("stdlib:         {}", jdk.stdlib.path().display());
        }
        None => println!("jdk home:       <none found>"),
    }
    println!("extra jars:     {}", project.classpath.len());

    let classpath_start = Instant::now();
    let mut db = Db::new(project);
    let classpath_ms = classpath_start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "classpath:      {} classes indexed in {:.1} ms",
        db.classpath().class_count(),
        classpath_ms
    );

    let files = db.project().discover_source_files();
    let discover_ms = load.elapsed().as_secs_f64() * 1000.0 - project_ms;
    let index_start = Instant::now();
    let mut bytes = 0usize;
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        bytes += text.len();
        db.index_parsed(path, SourceFile::new(text, 0));
    }
    let index_ms = index_start.elapsed().as_secs_f64() * 1000.0;

    println!(
        "sources:        {} files, {:.1} KiB, parsed and indexed in {:.1} ms",
        files.len(),
        bytes as f64 / 1024.0,
        index_ms
    );
    let stats = db.cache_stats();
    println!("workspace types:{}", stats.workspace_classes);

    // Lower every class, which is what the first completion in each file does.
    let lower_start = Instant::now();
    let mut classes = 0usize;
    let mut members = 0usize;
    for file in db.all_files() {
        for class in db.lowered_classes(file).iter() {
            classes += 1;
            members += class.fields.len() + class.methods.len();
        }
    }
    let lower_ms = lower_start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "lowered:        {classes} classes, {members} members in {:.1} ms",
        lower_ms
    );

    // Diagnostics across the workspace: the heaviest routine operation.
    let diagnostics_start = Instant::now();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut reported: Vec<(String, String)> = Vec::new();
    for file in db.all_files() {
        let items = java_ide::diagnostics::diagnostics(&db, file);
        for item in &items {
            match item.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => errors += 1,
                Some(lsp_types::DiagnosticSeverity::WARNING) => warnings += 1,
                _ => {}
            }
            if verbose && item.severity == Some(lsp_types::DiagnosticSeverity::ERROR) {
                // Relative to the project, so files of the same name in different modules differ.
                let name = db
                    .path(file)
                    .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
                    .unwrap_or_default();
                reported.push((
                    format!("{name}:{}", item.range.start.line + 1),
                    item.message.clone(),
                ));
            }
        }
    }
    let diagnostics_ms = diagnostics_start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "diagnostics:    {errors} errors, {warnings} warnings in {:.1} ms",
        diagnostics_ms
    );
    // FERRISJDT_SELFCHECK_LIMIT raises the number listed, for classifying a large project's errors.
    let limit = std::env::var("FERRISJDT_SELFCHECK_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);
    for (where_, message) in reported.iter().take(limit) {
        println!("  {where_}: {message}");
    }

    println!("project load:   {project_ms:.1} ms (discovery {discover_ms:.1} ms)");
    println!(
        "total:          {:.1} ms",
        total.elapsed().as_secs_f64() * 1000.0
    );
    if let Some(resident) = crate::memory::resident_bytes() {
        println!(
            "resident:       {:.1} MiB",
            resident as f64 / (1024.0 * 1024.0)
        );
    }
    if errors > 0 {
        1
    } else {
        0
    }
}

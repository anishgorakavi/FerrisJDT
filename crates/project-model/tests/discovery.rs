//! Tests for project layout detection and the source file walk.

use project_model::{Config, Project};
use std::fs;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("ferrisjdt-project-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn finds_sources_in_a_flat_directory() {
    let root = scratch("flat");
    fs::write(root.join("Main.java"), "class Main {}").unwrap();
    fs::write(root.join("Node.java"), "class Node {}").unwrap();
    fs::write(root.join("notes.txt"), "ignored").unwrap();
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files.iter().all(|f| f.extension().unwrap() == "java"));
}

#[test]
fn prefers_maven_layout_and_skips_build_output() {
    let root = scratch("maven");
    fs::create_dir_all(root.join("src/main/java/ds")).unwrap();
    fs::create_dir_all(root.join("target/classes/ds")).unwrap();
    fs::write(
        root.join("src/main/java/ds/Tree.java"),
        "package ds; class Tree {}",
    )
    .unwrap();
    fs::write(
        root.join("target/classes/ds/Generated.java"),
        "class Generated {}",
    )
    .unwrap();
    fs::write(root.join("pom.xml"), "<project/>").unwrap();
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("Tree.java"));
}

#[test]
fn skips_hidden_directories_and_symlinks() {
    let root = scratch("hidden");
    fs::create_dir_all(root.join(".hidden")).unwrap();
    fs::write(root.join(".hidden/Secret.java"), "class Secret {}").unwrap();
    fs::write(root.join("Real.java"), "class Real {}").unwrap();
    let outside = scratch("outside");
    fs::write(outside.join("Outside.java"), "class Outside {}").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    assert_eq!(
        files.len(),
        1,
        "a symlink must not pull in outside sources: {files:?}"
    );
    assert!(files[0].ends_with("Real.java"));
}

#[test]
fn honours_the_source_file_limit() {
    let root = scratch("limited");
    for i in 0..50 {
        fs::write(root.join(format!("C{i}.java")), "class C {}").unwrap();
    }
    let config = Config {
        max_source_files: Some(10),
        ..Config::default()
    };
    let project = Project::load(&root, &config);
    assert!(project.discover_source_files().len() <= 10);
}

#[test]
fn collects_jars_from_lib_and_config() {
    let root = scratch("classpath");
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/a.jar"), "").unwrap();
    fs::write(root.join("lib/readme.md"), "").unwrap();
    let project = Project::load(&root, &Config::default());
    assert_eq!(project.classpath.len(), 1);
    assert!(project.classpath[0].ends_with("a.jar"));
}

#[test]
fn deep_trees_terminate() {
    let root = scratch("deep");
    let mut path = root.clone();
    for i in 0..60 {
        path = path.join(format!("d{i}"));
    }
    fs::create_dir_all(&path).unwrap();
    fs::write(path.join("Deep.java"), "class Deep {}").unwrap();
    let project = Project::load(&root, &Config::default());
    // The walk stops at the depth limit rather than recursing without bound.
    assert!(project.discover_source_files().is_empty());
}

#[test]
fn discovers_a_jdk_with_a_readable_standard_library() {
    let root = scratch("jdk");
    let project = Project::load(&root, &Config::default());
    match &project.jdk {
        Some(jdk) => assert!(
            jdk.stdlib.path().is_file(),
            "the discovered standard library must exist"
        ),
        // A machine with no JDK is a valid configuration; the server degrades
        // to project-only analysis.
        None => eprintln!("no JDK installed; skipping"),
    }
}

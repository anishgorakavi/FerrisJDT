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

#[test]
fn reads_level_and_cached_classpath_from_a_gradle_project() {
    let root = scratch("gradle");
    fs::write(
        root.join("build.gradle"),
        "java {\n    toolchain {\n        languageVersion = JavaLanguageVersion.of(21)\n    }\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".ferrisjdt")).unwrap();
    let jar = root.join("dep.jar");
    fs::write(&jar, "").unwrap();
    fs::write(
        root.join(".ferrisjdt/classpath.txt"),
        format!("# generated\n{}\n/missing/gone.jar\n", jar.display()),
    )
    .unwrap();
    let project = Project::load(&root, &Config::default());
    assert_eq!(project.source_level.0, 21);
    assert_eq!(project.classpath, vec![jar]);
}

#[test]
fn explicit_level_beats_the_pom() {
    let root = scratch("pom-level");
    fs::write(
        root.join("pom.xml"),
        "<project><properties><java.version>17</java.version></properties></project>",
    )
    .unwrap();
    let detected = Project::load(&root, &Config::default());
    assert_eq!(detected.source_level.0, 17);
    let config = Config {
        source_level: Some(8),
        ..Config::default()
    };
    assert_eq!(Project::load(&root, &config).source_level.0, 8);
}

#[test]
fn opening_one_maven_module_indexes_its_siblings() {
    let root = scratch("multi-maven");
    fs::write(
        root.join("pom.xml"),
        "<project><modules><module>api</module><module>app</module></modules></project>",
    )
    .unwrap();
    for module in ["api", "app"] {
        fs::create_dir_all(root.join(module).join("src/main/java/p")).unwrap();
        fs::write(root.join(module).join("pom.xml"), "<project/>").unwrap();
    }
    fs::write(
        root.join("api/src/main/java/p/Dto.java"),
        "package p; class Dto {}",
    )
    .unwrap();
    fs::write(
        root.join("app/src/main/java/p/Main.java"),
        "package p; class Main {}",
    )
    .unwrap();
    let project = Project::load(&root.join("app"), &Config::default());
    assert_eq!(project.root, root);
    let files = project.discover_source_files();
    assert!(files.iter().any(|f| f.ends_with("Dto.java")), "{files:?}");
    assert!(files.iter().any(|f| f.ends_with("Main.java")), "{files:?}");
}

#[test]
fn opening_one_gradle_subproject_indexes_its_siblings() {
    let root = scratch("multi-gradle");
    fs::write(root.join("settings.gradle"), "include 'core', 'web'\n").unwrap();
    for module in ["core", "web"] {
        fs::create_dir_all(root.join(module).join("src/main/java")).unwrap();
        fs::write(root.join(module).join("build.gradle"), "").unwrap();
    }
    fs::write(root.join("core/src/main/java/Core.java"), "class Core {}").unwrap();
    fs::write(root.join("web/src/main/java/Web.java"), "class Web {}").unwrap();
    let project = Project::load(&root.join("web"), &Config::default());
    let files = project.discover_source_files();
    assert!(files.iter().any(|f| f.ends_with("Core.java")), "{files:?}");
}

#[test]
fn an_unrelated_parent_build_file_does_not_widen_the_root() {
    let root = scratch("not-a-module");
    fs::write(
        root.join("pom.xml"),
        "<project><modules><module>other</module></modules></project>",
    )
    .unwrap();
    fs::create_dir_all(root.join("mine")).unwrap();
    let project = Project::load(&root.join("mine"), &Config::default());
    assert_eq!(project.root, root.join("mine"));
}

#[test]
fn indexes_generated_sources_listed_by_the_client() {
    let root = scratch("generated");
    fs::create_dir_all(root.join("src/main/java/p")).unwrap();
    fs::write(root.join("src/main/java/p/A.java"), "package p; class A {}").unwrap();
    // Annotation processor output sits under `build`, which the ordinary walk skips.
    let generated = root.join("build/generated/sources/annotationProcessor/java/main/p");
    fs::create_dir_all(&generated).unwrap();
    fs::write(
        generated.join("AMapperImpl.java"),
        "package p; class AMapperImpl {}",
    )
    .unwrap();
    fs::create_dir_all(root.join(".ferrisjdt")).unwrap();
    fs::write(
        root.join(".ferrisjdt/sources.txt"),
        format!(
            "{}\n{}\n/missing/dir\n",
            root.join("src/main/java").display(),
            root.join("build/generated/sources/annotationProcessor/java/main")
                .display()
        ),
    )
    .unwrap();
    let project = Project::load(&root, &Config::default());
    let files = project.discover_source_files();
    assert!(files.iter().any(|f| f.ends_with("A.java")), "{files:?}");
    assert!(
        files.iter().any(|f| f.ends_with("AMapperImpl.java")),
        "{files:?}"
    );
    assert_eq!(files.len(), 2, "no duplicates: {files:?}");
}

#[test]
fn files_the_build_excludes_are_not_indexed() {
    let root = scratch("excluded");
    fs::create_dir_all(root.join("src/test/java/p")).unwrap();
    fs::write(
        root.join("src/test/java/p/KeepTest.java"),
        "package p; class KeepTest {}",
    )
    .unwrap();
    let skipped = root.join("src/test/java/p/ProdOnly.java");
    fs::write(&skipped, "package p; class ProdOnly {}").unwrap();
    fs::create_dir_all(root.join(".ferrisjdt")).unwrap();
    fs::write(
        root.join(".ferrisjdt/excluded.txt"),
        format!("{}\n", skipped.display()),
    )
    .unwrap();
    let files = Project::load(&root, &Config::default()).discover_source_files();
    assert!(
        files.iter().any(|f| f.ends_with("KeepTest.java")),
        "{files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with("ProdOnly.java")),
        "{files:?}"
    );
}

#[test]
fn the_level_a_client_read_from_the_build_tool_wins_over_build_file_text() {
    let root = scratch("level-file");
    // A convention plugin elsewhere sets the real level; the root build file says nothing.
    fs::write(root.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
    fs::create_dir_all(root.join(".ferrisjdt")).unwrap();
    fs::write(root.join(".ferrisjdt/level.txt"), "25\n").unwrap();
    assert_eq!(Project::load(&root, &Config::default()).source_level.0, 25);
    let config = Config {
        source_level: Some(11),
        ..Config::default()
    };
    assert_eq!(Project::load(&root, &config).source_level.0, 11);
}

#[test]
fn a_jdk_home_is_matched_by_its_release_file_not_its_name() {
    use project_model::jdk;
    let root = scratch("jdk-release");
    // A home named like Java 25 whose release file says 17 must not count as 25.
    let home = root.join("jdk-25");
    fs::create_dir_all(home.join("lib")).unwrap();
    fs::write(home.join("release"), "JAVA_VERSION=\"17.0.2\"\n").unwrap();
    fs::write(home.join("lib/ct.sym"), "").unwrap();
    assert_eq!(jdk::home_major(&home), Some(17));
    let old = root.join("jdk8");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("release"), "JAVA_VERSION=\"1.8.0_402\"\n").unwrap();
    assert_eq!(jdk::home_major(&old), Some(8));
}

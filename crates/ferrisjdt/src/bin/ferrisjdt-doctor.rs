//! `ferrisjdt-doctor`: checks the environment FerrisJDT runs in and says what
//! is wrong, and how to fix it.
//!
//! The most common failure is a missing or unreadable JDK: without a standard
//! library every `java.lang` type (`String`, `Comparable`, ...) is reported as
//! an unresolved symbol. The doctor reproduces the server's own discovery and
//! indexing, so what it reports is what the server would see.

use java_semantic::Db;
use project_model::{jdk, Project, StdlibSource};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
ferrisjdt-doctor: diagnose a FerrisJDT setup

USAGE:
    ferrisjdt-doctor [DIR] [--json] [--verbose]

ARGS:
    DIR         project to check (default: the current directory)

OPTIONS:
    --json      machine-readable report on stdout
    -v, --verbose
                list every JDK location that was searched
    -V, --version
    -h, --help

Exit status is 1 when a check fails, otherwise 0. Warnings do not fail.
";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

struct Check {
    level: Level,
    name: &'static str,
    detail: String,
    hint: Option<String>,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn add(&mut self, level: Level, name: &'static str, detail: impl Into<String>) {
        self.checks.push(Check {
            level,
            name,
            detail: detail.into(),
            hint: None,
        });
    }

    fn add_hint(
        &mut self,
        level: Level,
        name: &'static str,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) {
        self.checks.push(Check {
            level,
            name,
            detail: detail.into(),
            hint: Some(hint.into()),
        });
    }

    fn count(&self, level: Level) -> usize {
        self.checks.iter().filter(|c| c.level == level).count()
    }
}

fn main() -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut as_json = false;
    let mut verbose = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("ferrisjdt-doctor {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--json" => as_json = true,
            "-v" | "--verbose" => verbose = true,
            other if other.starts_with('-') => {
                eprintln!("unknown option: {other}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
            other => dir = Some(PathBuf::from(other)),
        }
    }

    let root = dir.unwrap_or_else(|| PathBuf::from("."));
    let root = match root.canonicalize() {
        Ok(path) => strip_verbatim(path),
        Err(error) => {
            eprintln!("cannot read {}: {error}", root.display());
            return ExitCode::FAILURE;
        }
    };

    let mut report = Report::default();
    run_checks(&root, verbose, &mut report);

    if as_json {
        print_json(&root, &report);
    } else {
        print_text(&root, &report);
    }
    if report.count(Level::Fail) > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_checks(root: &Path, verbose: bool, report: &mut Report) {
    report.add(
        Level::Info,
        "ferrisjdt",
        format!(
            "doctor {} on {}/{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    check_server_binary(report);
    check_environment(report);
    let config = check_config(root, report);

    let release = config.source_level.unwrap_or(8);
    let project = Project::load(root, &config);
    check_jdk(&project, &config, release, verbose, report);
    check_project(&project, report);
    check_classpath(project, report);
}

/// The server binary should be installed next to the doctor, or on `PATH`.
fn check_server_binary(report: &mut Report) {
    let exe = if cfg!(windows) {
        "ferrisjdt.exe"
    } else {
        "ferrisjdt"
    };
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe)))
        .filter(|p| p.is_file());
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(exe))
            .find(|p| p.is_file())
    });
    match (beside, on_path) {
        (_, Some(path)) => report.add(Level::Ok, "server binary", path.display().to_string()),
        (Some(path), None) => report.add_hint(
            Level::Warn,
            "server binary",
            format!("{} is not on PATH", path.display()),
            "editors launch `ferrisjdt` by name; add its directory to PATH or set the \
             editor's server path explicitly",
        ),
        (None, None) => report.add_hint(
            Level::Warn,
            "server binary",
            format!("`{exe}` not found on PATH or beside the doctor"),
            "install it with `cargo install --path crates/ferrisjdt`",
        ),
    }
}

fn check_environment(report: &mut Report) {
    match std::env::var_os("JAVA_HOME") {
        None => report.add(
            Level::Info,
            "JAVA_HOME",
            "not set (fine: FerrisJDT also searches PATH and standard install directories)",
        ),
        Some(home) => {
            let home = PathBuf::from(home);
            if !home.is_dir() {
                report.add_hint(
                    Level::Warn,
                    "JAVA_HOME",
                    format!("{} is not a directory", home.display()),
                    "point JAVA_HOME at a JDK, or unset it",
                );
            } else if jdk::stdlib_in(&home, 8).is_none() {
                report.add_hint(
                    Level::Warn,
                    "JAVA_HOME",
                    format!(
                        "{} has no jre/lib/rt.jar or lib/ct.sym, so it is not a full JDK",
                        home.display()
                    ),
                    "a JRE is not enough; install a JDK, or set \"jdkHome\" in ferrisjdt.json",
                );
            } else {
                report.add(Level::Ok, "JAVA_HOME", home.display().to_string());
            }
        }
    }
    match std::env::var("FERRISJDT_LOG") {
        Ok(value) => report.add(Level::Info, "FERRISJDT_LOG", value),
        Err(_) => report.add(
            Level::Info,
            "FERRISJDT_LOG",
            "not set (set to `debug` for more detail on stderr)",
        ),
    }
}

/// Reads `ferrisjdt.json` strictly: the server ignores a malformed file
/// silently, which is exactly what the doctor should surface.
fn check_config(root: &Path, report: &mut Report) -> project_model::Config {
    let path = root.join("ferrisjdt.json");
    if !path.is_file() {
        report.add(Level::Info, "ferrisjdt.json", "none (defaults in use)");
        return project_model::Config::default();
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            report.add(Level::Warn, "ferrisjdt.json", format!("unreadable: {error}"));
            return project_model::Config::default();
        }
    };
    match serde_json::from_str::<project_model::Config>(&text) {
        Ok(config) => {
            report.add(Level::Ok, "ferrisjdt.json", "parsed");
            config
        }
        Err(error) => {
            report.add_hint(
                Level::Warn,
                "ferrisjdt.json",
                format!("malformed, so the server ignores it: {error}"),
                "fix the JSON; keys are jdkHome, sourceLevel, sourceRoots, classpath, \
                 exclude, maxSourceFiles",
            );
            project_model::Config::default()
        }
    }
}

fn describe(source: &StdlibSource) -> String {
    match source {
        StdlibSource::RtJar(path) => format!("rt.jar ({})", path.display()),
        StdlibSource::CtSym { path, release } => {
            format!("ct.sym for release {release} ({})", path.display())
        }
    }
}

fn check_jdk(
    project: &Project,
    config: &project_model::Config,
    release: u32,
    verbose: bool,
    report: &mut Report,
) {
    if let Some(configured) = &config.jdk_home {
        if jdk::stdlib_in(configured, release).is_none() {
            report.add_hint(
                Level::Warn,
                "jdkHome",
                format!(
                    "{} (from ferrisjdt.json) has no readable standard library; falling back \
                     to discovery",
                    configured.display()
                ),
                "fix the path, or remove \"jdkHome\"",
            );
        }
    }

    if verbose {
        let candidates = jdk::candidate_homes();
        if candidates.is_empty() {
            report.add(Level::Info, "searched", "no candidate JDK directories exist");
        }
        let mut rejected = 0usize;
        for (origin, home) in candidates {
            match jdk::stdlib_in(&home, release) {
                Some(source) => report.add(
                    Level::Info,
                    "searched",
                    format!("[{origin}] {}: usable, {}", home.display(), describe(&source)),
                ),
                // Most entries under a vendor folder are not JDKs at all.
                None if origin == "JAVA_HOME" || origin == "PATH" => report.add(
                    Level::Info,
                    "searched",
                    format!("[{origin}] {}: no standard library", home.display()),
                ),
                None => rejected += 1,
            }
        }
        if rejected > 0 {
            report.add(
                Level::Info,
                "searched",
                format!("{rejected} other directories under install roots held no JDK"),
            );
        }
    }

    match &project.jdk {
        Some(found) => {
            report.add(
                Level::Ok,
                "JDK",
                format!("{} (source level {release})", found.home.display()),
            );
            report.add(Level::Ok, "standard library", describe(&found.stdlib));
            if found.sources.is_none() {
                report.add_hint(
                    Level::Info,
                    "src.zip",
                    "not found",
                    "without it, hover shows no javadoc and parameter names for JDK methods",
                );
            }
        }
        None => report.add_hint(
            Level::Fail,
            "JDK",
            "none found: String, Comparable, List and every other JDK type will be reported \
             as unresolved symbols",
            install_hint(),
        ),
    }
}

fn install_hint() -> &'static str {
    if cfg!(windows) {
        "install a JDK (for example Eclipse Temurin), then either set JAVA_HOME to its folder \
         or put \"jdkHome\" in ferrisjdt.json. Re-run with --verbose to see where was searched"
    } else if cfg!(target_os = "macos") {
        "install a JDK (for example `brew install openjdk`), or set JAVA_HOME or \"jdkHome\" \
         in ferrisjdt.json. Re-run with --verbose to see where was searched"
    } else {
        "install a JDK package (for example openjdk-21-jdk), or set JAVA_HOME or \"jdkHome\" \
         in ferrisjdt.json. Re-run with --verbose to see where was searched"
    }
}

fn check_project(project: &Project, report: &mut Report) {
    let roots: Vec<String> = project
        .source_roots
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    report.add(Level::Info, "source roots", roots.join(", "));

    let files = project.discover_source_files();
    if files.is_empty() {
        report.add_hint(
            Level::Warn,
            "source files",
            "no .java files found",
            "run the doctor from the project folder, or add \"sourceRoots\" in ferrisjdt.json",
        );
    } else if files.len() >= project.max_source_files {
        report.add_hint(
            Level::Warn,
            "source files",
            format!("{} (hit the limit; later files are not indexed)", files.len()),
            "raise \"maxSourceFiles\" or add directories to \"exclude\"",
        );
    } else {
        report.add(Level::Ok, "source files", files.len().to_string());
    }

    if project.classpath.is_empty() {
        report.add(Level::Info, "extra jars", "none (lib/, libs/, jars/ are scanned)");
    } else {
        let unreadable: Vec<String> = project
            .classpath
            .iter()
            .filter(|p| std::fs::File::open(p).is_err())
            .map(|p| p.display().to_string())
            .collect();
        if unreadable.is_empty() {
            report.add(
                Level::Ok,
                "extra jars",
                format!("{} readable", project.classpath.len()),
            );
        } else {
            report.add(
                Level::Warn,
                "extra jars",
                format!("unreadable: {}", unreadable.join(", ")),
            );
        }
    }
}

/// Indexes the classpath exactly as the server does, then looks up the types
/// whose absence causes the "unrecognized symbol" reports.
fn check_classpath(project: Project, report: &mut Report) {
    if project.jdk.is_none() && project.classpath.is_empty() {
        report.add(Level::Info, "classpath", "skipped (no JDK)");
        return;
    }
    let start = Instant::now();
    let db = Db::new(project);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let classpath = db.classpath();
    report.add(
        Level::Info,
        "classpath",
        format!("{} classes indexed in {ms:.0} ms", classpath.class_count()),
    );

    let probes = [
        "java.lang.Object",
        "java.lang.String",
        "java.lang.Comparable",
        "java.util.List",
        "java.util.Map",
    ];
    let missing: Vec<&str> = probes
        .iter()
        .copied()
        .filter(|name| !classpath.contains(name) || classpath.class(name).is_none())
        .collect();
    if missing.is_empty() {
        report.add(
            Level::Ok,
            "JDK types",
            "Object, String, Comparable, List and Map all resolve",
        );
    } else {
        report.add_hint(
            Level::Fail,
            "JDK types",
            format!("cannot load {}", missing.join(", ")),
            "the standard library was found but is unreadable or lacks these classes; try \
             another JDK via \"jdkHome\" in ferrisjdt.json",
        );
    }
}

fn print_text(root: &Path, report: &Report) {
    println!("ferrisjdt-doctor: {}\n", root.display());
    for check in &report.checks {
        println!("[{:>4}] {:<17} {}", check.level.tag(), check.name, check.detail);
        if let Some(hint) = &check.hint {
            println!("       {:<17} -> {hint}", "");
        }
    }
    let (fails, warns) = (report.count(Level::Fail), report.count(Level::Warn));
    println!();
    if fails > 0 {
        println!("{fails} problem(s) need fixing, {warns} warning(s).");
    } else if warns > 0 {
        println!("No failures, {warns} warning(s).");
    } else {
        println!("Everything looks healthy.");
    }
}

fn print_json(root: &Path, report: &Report) {
    let checks: Vec<_> = report
        .checks
        .iter()
        .map(|c| {
            json!({
                "level": c.level.tag().to_lowercase(),
                "name": c.name,
                "detail": c.detail,
                "hint": c.hint,
            })
        })
        .collect();
    let value = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "root": root.display().to_string(),
        "healthy": report.count(Level::Fail) == 0,
        "failures": report.count(Level::Fail),
        "warnings": report.count(Level::Warn),
        "checks": checks,
    });
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
}

/// `canonicalize` yields `\\?\C:\...` on Windows, which reads badly.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

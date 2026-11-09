//! Locates a JDK and the archive that holds its standard library.
//!
//! No JVM is ever started. A JDK 8 layout exposes the whole library as
//! `jre/lib/rt.jar`, which is an ordinary JAR of class files. JDK 9 and later
//! replaced it with a `jimage` container that is not a ZIP, but they ship
//! `lib/ct.sym` — the archive `javac --release` reads — which does contain
//! per-release class files and is a ZIP, so it serves as the fallback.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdlibSource {
    /// A JDK 8 `rt.jar`: entries are `java/util/List.class`.
    RtJar(PathBuf),
    /// A JDK 9+ `ct.sym`: entries are `<releases>/java/util/List.sig`, where
    /// `releases` is a string of base-36 release characters (`8` is "8", 17 is "H").
    CtSym { path: PathBuf, release: u32 },
}

impl StdlibSource {
    pub fn path(&self) -> &Path {
        match self {
            StdlibSource::RtJar(p) => p,
            StdlibSource::CtSym { path, .. } => path,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Jdk {
    pub home: PathBuf,
    pub stdlib: StdlibSource,
    /// `src.zip`, when present: the source of javadoc and parameter names for
    /// standard library members, which class files do not carry.
    pub sources: Option<PathBuf>,
}

/// Finds the standard library inside a JDK home for the requested release.
pub fn stdlib_in(home: &Path, release: u32) -> Option<StdlibSource> {
    for candidate in [home.join("jre/lib/rt.jar"), home.join("lib/rt.jar")] {
        if candidate.is_file() {
            return Some(StdlibSource::RtJar(candidate));
        }
    }
    let ct_sym = home.join("lib/ct.sym");
    if ct_sym.is_file() {
        return Some(StdlibSource::CtSym {
            path: ct_sym,
            release,
        });
    }
    None
}

fn sources_in(home: &Path) -> Option<PathBuf> {
    [home.join("src.zip"), home.join("lib/src.zip")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn jdk_at(home: &Path, release: u32) -> Option<Jdk> {
    let stdlib = stdlib_in(home, release)?;
    Some(Jdk {
        home: home.to_path_buf(),
        stdlib,
        sources: sources_in(home),
    })
}

/// Searches for a JDK, preferring one whose own release matches `release`
/// (so a Java 8 project reads a real `rt.jar` when one is installed).
///
/// Order: the explicit override, `JAVA_HOME`, `/usr/libexec/java_home` on
/// macOS, a JDK on `PATH`, then conventional Linux and Windows install
/// directories.
pub fn discover(explicit: Option<&Path>, release: u32) -> Option<Jdk> {
    if let Some(home) = explicit {
        if let Some(jdk) = jdk_at(home, release) {
            return Some(jdk);
        }
        tracing::warn!(path = %home.display(), "configured jdkHome has no readable standard library");
    }

    let mut fallback: Option<Jdk> = None;
    let mut consider = |home: &Path, exact: bool| -> Option<Jdk> {
        let jdk = jdk_at(home, release)?;
        let is_rt_jar = matches!(jdk.stdlib, StdlibSource::RtJar(_));
        // The home's own `release` file is authoritative: `java_home -v 25` happily answers with a
        // JDK 21, so a name or a tool's say-so is only a guess.
        let exact = match home_major(home) {
            Some(major) => major == release,
            None => exact,
        };
        // A JDK of exactly the requested release is the best possible match: its `rt.jar` for 8,
        // its `lib/modules` for the release `ct.sym` omits.
        if exact && (is_rt_jar || release >= 9) {
            return Some(jdk);
        }
        if fallback.is_none() {
            fallback = Some(jdk);
        }
        None
    };

    // Homebrew keeps each release in its own keg that `java_home` does not list.
    if cfg!(target_os = "macos") {
        for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
            let home = PathBuf::from(format!(
                "{prefix}/openjdk@{release}/libexec/openjdk.jdk/Contents/Home"
            ));
            if home.is_dir() {
                if let Some(jdk) = consider(&home, true) {
                    return Some(jdk);
                }
            }
        }
    }
    // `java_home -v <release>` asks macOS for that release.
    if cfg!(target_os = "macos") {
        let version = if release == 8 {
            "1.8".to_string()
        } else {
            release.to_string()
        };
        if let Some(home) = java_home_tool(Some(&version)) {
            if let Some(jdk) = consider(&home, true) {
                return Some(jdk);
            }
        }
    }
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let home = PathBuf::from(home);
        let exact = home_looks_like_release(&home, release);
        if let Some(jdk) = consider(&home, exact) {
            return Some(jdk);
        }
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = java_home_tool(None) {
            if let Some(jdk) = consider(&home, false) {
                return Some(jdk);
            }
        }
    }
    // A JDK on `PATH` (e.g. `<home>/bin/javac`), for machines with no JAVA_HOME.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let Some(home) = dir.parent() else { continue };
            if dir.file_name().is_some_and(|n| n == "bin")
                && (dir.join("javac").is_file() || dir.join("javac.exe").is_file())
            {
                let exact = home_looks_like_release(home, release);
                if let Some(jdk) = consider(home, exact) {
                    return Some(jdk);
                }
            }
        }
    }
    for root in install_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut homes: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        homes.sort();
        for home in homes {
            let exact = home_looks_like_release(&home, release);
            if let Some(jdk) = consider(&home, exact) {
                return Some(jdk);
            }
        }
    }
    fallback
}

/// Directories whose children are JDK homes.
fn install_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["/usr/lib/jvm", "/usr/java", "/opt/java"]
        .iter()
        .map(PathBuf::from)
        .collect();
    if cfg!(windows) {
        for var in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "LOCALAPPDATA",
        ] {
            let Some(base) = std::env::var_os(var) else {
                continue;
            };
            let base = PathBuf::from(base);
            for vendor in [
                "Java",
                "Eclipse Adoptium",
                "Microsoft",
                "Amazon Corretto",
                "Zulu",
                "BellSoft",
                "Semeru",
                "Programs/Eclipse Adoptium",
            ] {
                roots.push(base.join(vendor));
            }
        }
    }
    roots
}

/// Every JDK home the search would look at, labelled by where it came from,
/// whether or not it holds a usable standard library. For diagnostics.
pub fn candidate_homes() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        out.push(("JAVA_HOME".to_string(), PathBuf::from(home)));
    }
    if cfg!(target_os = "macos") {
        if let Some(home) = java_home_tool(None) {
            out.push(("java_home".to_string(), home));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(home) = dir.parent() {
                if dir.file_name().is_some_and(|n| n == "bin")
                    && (dir.join("javac").is_file() || dir.join("javac.exe").is_file())
                {
                    out.push(("PATH".to_string(), home.to_path_buf()));
                }
            }
        }
    }
    for root in install_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut homes: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        homes.sort();
        for home in homes {
            out.push((root.display().to_string(), home));
        }
    }
    // The same directory can be reached through several roots.
    let mut seen = Vec::new();
    out.retain(|(_, home)| {
        let fresh = !seen.contains(home);
        seen.push(home.clone());
        fresh
    });
    out
}

/// The major Java version in a JDK home's `release` file: `JAVA_VERSION="21.0.1"` is 21 and
/// `"1.8.0_402"` is 8.
pub fn home_major(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let line = text.lines().find(|l| l.starts_with("JAVA_VERSION="))?;
    let version = line.trim_start_matches("JAVA_VERSION=").trim_matches('"');
    let mut parts = version.split(['.', '-', '+', '_']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

fn home_looks_like_release(home: &Path, release: u32) -> bool {
    let name = home.to_string_lossy();
    if release == 8 {
        name.contains("1.8") || name.contains("-8") || name.contains("jdk8") || name.contains("@8")
    } else {
        name.contains(&format!("-{release}"))
            || name.contains(&format!("@{release}"))
            || name.contains(&format!("jdk{release}"))
            || name.contains(&format!("{release}."))
    }
}

/// Runs macOS's `java_home`, which only reports installed JDK locations and
/// never executes project code.
fn java_home_tool(version: Option<&str>) -> Option<PathBuf> {
    let tool = Path::new("/usr/libexec/java_home");
    if !tool.is_file() {
        return None;
    }
    let mut command = Command::new(tool);
    if let Some(version) = version {
        command.args(["-v", version]);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_dir().then_some(path)
}

/// Encodes a release number the way `ct.sym` directory names do: releases are
/// single base-36 characters, so 8 is `8` and 17 is `H`.
pub fn ct_sym_release_char(release: u32) -> char {
    std::char::from_digit(release, 36)
        .unwrap_or('8')
        .to_ascii_uppercase()
}

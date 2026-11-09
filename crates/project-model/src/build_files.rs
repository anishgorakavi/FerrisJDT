//! What Maven and Gradle projects say about themselves, read from files only.
//!
//! The build tools are never run here: a client resolves dependencies and writes
//! `.ferrisjdt/classpath.txt`, and the Java level is read out of the build file's text.

use std::path::{Path, PathBuf};

/// Where a client leaves the resolved dependency JARs, one path per line.
pub const CLASSPATH_FILE: &str = ".ferrisjdt/classpath.txt";

/// Reads the resolved classpath a client cached for this project. Blank lines and
/// `#` comments are skipped, and paths that no longer exist are dropped.
pub fn read_classpath_file(root: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(root.join(CLASSPATH_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

/// The outermost directory whose build file declares `root` as a module, or `root` itself.
///
/// A client that opens one module of a multi-module project reports that module as the workspace,
/// yet its siblings' classes are what it depends on, so the parent project is the real root.
pub fn enclosing_build_root(root: &Path) -> PathBuf {
    let mut best = root.to_path_buf();
    let mut child = root.to_path_buf();
    for ancestor in root.ancestors().skip(1).take(8) {
        if !declares_module(ancestor, &child) {
            break;
        }
        best = ancestor.to_path_buf();
        child = ancestor.to_path_buf();
    }
    best
}

/// True when `parent`'s `pom.xml` or `settings.gradle[.kts]` names `child` as a module.
fn declares_module(parent: &Path, child: &Path) -> bool {
    let Ok(relative) = child.strip_prefix(parent) else {
        return false;
    };
    let path = relative.to_string_lossy().replace('\\', "/");
    if path.is_empty() {
        return false;
    }
    if let Ok(pom) = std::fs::read_to_string(parent.join("pom.xml")) {
        if pom.contains(&format!("<module>{path}</module>")) {
            return true;
        }
    }
    let gradle_path = path.replace('/', ":");
    ["settings.gradle", "settings.gradle.kts"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(parent.join(name)).ok())
        .any(|text| {
            ["'", "\""].iter().any(|quote| {
                text.contains(&format!("{quote}{gradle_path}{quote}"))
                    || text.contains(&format!("{quote}:{gradle_path}{quote}"))
            })
        })
}

/// The Java release a Maven or Gradle build asks for, if its file says so plainly.
pub fn detect_source_level(root: &Path) -> Option<u32> {
    if let Ok(pom) = std::fs::read_to_string(root.join("pom.xml")) {
        if let Some(level) = pom_level(&pom) {
            return Some(level);
        }
    }
    ["build.gradle", "build.gradle.kts"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .find_map(|text| gradle_level(&text))
}

/// Finds the first Java-level property or compiler plugin setting in a `pom.xml`.
fn pom_level(pom: &str) -> Option<u32> {
    const TAGS: &[&str] = &[
        "maven.compiler.release",
        "java.version",
        "maven.compiler.source",
        "release",
        "source",
    ];
    TAGS.iter().find_map(|tag| {
        let open = format!("<{tag}>");
        let start = pom.find(&open)? + open.len();
        let end = pom[start..].find('<')? + start;
        parse_level(&pom[start..end])
    })
}

/// Finds a toolchain or compatibility setting in a `build.gradle` or `.kts` file.
fn gradle_level(text: &str) -> Option<u32> {
    const KEYS: &[&str] = &[
        "JavaLanguageVersion.of(",
        "sourceCompatibility",
        "targetCompatibility",
        "options.release",
    ];
    text.lines().find_map(|line| {
        let line = line.split("//").next().unwrap_or("");
        KEYS.iter().find_map(|key| {
            let rest = &line[line.find(key)? + key.len()..];
            parse_level(rest)
        })
    })
}

/// Reads a release number out of `17`, `1.8`, `'11'`, or `JavaVersion.VERSION_1_8`.
fn parse_level(text: &str) -> Option<u32> {
    let mut numbers = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u32>().ok());
    let first = numbers.next()?;
    let level = if first == 1 { numbers.next()? } else { first };
    (5..=99).contains(&level).then_some(level)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spellings real Maven and Gradle projects use all reduce to a release number.
    #[test]
    fn parses_common_spellings() {
        assert_eq!(parse_level("1.8"), Some(8));
        assert_eq!(parse_level(" = JavaVersion.VERSION_1_8"), Some(8));
        assert_eq!(parse_level("'17'"), Some(17));
        assert_eq!(parse_level(" = JavaVersion.VERSION_21"), Some(21));
        assert_eq!(parse_level("${foo}"), None);
    }
}

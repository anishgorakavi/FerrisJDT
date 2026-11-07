//! Identifier and qualified-name handling: binary, simple, and source forms.

use std::sync::Arc;

/// An identifier or qualified name. `Arc<str>` keeps cloning cheap while
/// leaving lookups allocation-free, since `Arc<str>` borrows as `str`.
pub type Name = Arc<str>;

pub fn name(text: &str) -> Name {
    Arc::from(text)
}

/// The last segment of a binary name: `java.util.Map$Entry` yields `Entry`.
pub fn simple_name(binary: &str) -> &str {
    let after_package = match binary.rfind('.') {
        Some(at) => &binary[at + 1..],
        None => binary,
    };
    match after_package.rfind('$') {
        // A `$` followed by a digit marks an anonymous class, which has no
        // simple name of its own.
        Some(at)
            if after_package[at + 1..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_ascii_digit()) =>
        {
            &after_package[at + 1..]
        }
        _ => after_package,
    }
}

/// The package of a binary name, or `""` for the default package.
pub fn package_of(binary: &str) -> &str {
    match binary.rfind('.') {
        Some(at) => &binary[..at],
        None => "",
    }
}

/// The source-level spelling of a binary name: `java.util.Map$Entry` becomes
/// `java.util.Map.Entry`.
pub fn source_name(binary: &str) -> String {
    binary.replace('$', ".")
}

/// The enclosing class of a nested binary name, if any.
pub fn enclosing_class(binary: &str) -> Option<&str> {
    let at = binary.rfind('$')?;
    Some(&binary[..at])
}

/// True for names the standard library keeps to itself; they resolve when
/// named explicitly but are never offered as completions.
pub fn is_internal(binary: &str) -> bool {
    const INTERNAL_PREFIXES: &[&str] = &[
        "com.sun.",
        "sun.",
        "jdk.internal.",
        "jdk.nashorn.internal.",
        "oracle.",
        "netscape.",
        "org.jcp.",
        "apple.",
    ];
    INTERNAL_PREFIXES.iter().any(|p| binary.starts_with(p))
        // Anonymous and synthetic nested classes.
        || binary.rsplit('$').next().is_some_and(|last| last.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

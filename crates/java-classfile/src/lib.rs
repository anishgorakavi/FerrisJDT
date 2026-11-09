//! Native reader for Java class files and JAR archives.
//!
//! This crate is what lets the server understand the JDK and third-party
//! libraries without starting a JVM. It is deliberately standalone: it knows
//! about descriptors and bytes, not about classpaths or resolved types.

pub mod class;
pub mod constant_pool;
pub mod error;
pub mod jar;
pub mod jimage;
pub mod reader;
pub mod signature;

pub use class::{flags, ClassInfo, FieldInfo, InnerClassInfo, MethodInfo};
pub use error::{ClassFileError, Result};
pub use jar::Jar;
pub use jimage::JImage;
pub use signature::{ClassSig, MethodSig, SigArg, SigType, TypeParamSig};

/// Converts an internal JVM name (`java/util/Map$Entry`) into the source-level
/// qualified name (`java.util.Map.Entry`).
pub fn internal_to_qualified(internal: &str) -> String {
    internal.replace(['/', '$'], ".")
}

/// Converts an internal JVM name into its binary name (`java.util.Map$Entry`),
/// which is what a classpath lookup keys on.
pub fn internal_to_binary(internal: &str) -> String {
    internal.replace('/', ".")
}

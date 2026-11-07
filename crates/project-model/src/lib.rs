//! Project discovery: source roots, language level, JDK location, classpath.

pub mod config;
pub mod jdk;
pub mod project;

pub use config::{Config, SourceLevel};
pub use jdk::{Jdk, StdlibSource};
pub use project::{load_config_file, Project};

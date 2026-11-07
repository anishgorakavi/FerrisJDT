//! Java semantic analysis: symbol tables, types, and name resolution.
//!
//! The entry point is [`db::Db`], which owns file contents, the workspace
//! index, and the classpath, and answers every query the IDE layer asks.

pub mod attach;
pub mod classpath;
pub mod db;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod javadoc;
pub mod lower;
pub mod members;
pub mod name;
pub mod outline;
pub mod resolve;
pub mod symbols;
pub mod types;

pub use db::{CacheStats, Db};
pub use members::{LookupOptions, Members};
pub use name::Name;
pub use symbols::{ClassData, ClassKind, ClassRef, FileId, MethodData, Origin, Visibility};
pub use types::{Primitive, Substitution, Type};

/// Hard limit on how far resolution will walk a type hierarchy.
///
/// Java forbids cyclic inheritance, but a half-typed or hostile file can still
/// describe one, and the visited-set guards in each walker are backed up by
/// this bound so no query can run away.
pub const MAX_HIERARCHY_DEPTH: u32 = 64;

/// Hard limit on recursion while inferring the type of an expression.
pub const MAX_EXPRESSION_DEPTH: u32 = 128;
pub mod infer;
pub mod overload;
pub mod scope;

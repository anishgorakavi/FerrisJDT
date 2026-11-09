//! IDE features: completion, hover, navigation, diagnostics, and refactorings.
//!
//! Each module is one LSP capability and depends only on `java-semantic` for
//! facts and on `lsp-types` for shapes. Nothing here re-implements analysis.

pub mod code_actions;
pub mod completion;
mod constants;
pub mod convert;
pub mod diagnostics;
pub mod hover;
pub mod imports;
pub mod navigation;
pub mod rename;
pub mod render;
pub mod signature;
pub mod symbols;
pub mod target;
pub mod tokens;
pub mod uri;

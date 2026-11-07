//! Semantic tokens.
//!
//! Highlighting is driven by resolution, not by the grammar: a name is
//! classified by what it resolves to, which is what distinguishes a field from
//! a local and a class from a type parameter.

use crate::target::{self, Target};
use java_semantic::scope;
use java_semantic::symbols::FileId;
use java_semantic::Db;
use java_syntax::Node;
use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens};

/// Token types this server emits, in the order the client is told about them.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::CLASS,
    SemanticTokenType::INTERFACE,
    SemanticTokenType::ENUM,
    SemanticTokenType::TYPE_PARAMETER,
    SemanticTokenType::PARAMETER,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::METHOD,
    SemanticTokenType::NAMESPACE,
];

pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::STATIC,
    SemanticTokenModifier::READONLY,
    SemanticTokenModifier::DEPRECATED,
    SemanticTokenModifier::DECLARATION,
];

mod index {
    pub const CLASS: u32 = 0;
    pub const INTERFACE: u32 = 1;
    pub const ENUM: u32 = 2;
    pub const TYPE_PARAMETER: u32 = 3;
    pub const PARAMETER: u32 = 4;
    pub const VARIABLE: u32 = 5;
    pub const PROPERTY: u32 = 6;
    pub const METHOD: u32 = 7;
    pub const NAMESPACE: u32 = 8;
}

mod modifier {
    pub const STATIC: u32 = 1 << 0;
    pub const READONLY: u32 = 1 << 1;
    pub const DEPRECATED: u32 = 1 << 2;
    pub const DECLARATION: u32 = 1 << 3;
}

/// Cap on tokens per file, so a large file cannot produce an unbounded reply.
const MAX_TOKENS: usize = 20_000;

pub fn semantic_tokens(db: &Db, file: FileId) -> Option<SemanticTokens> {
    let source = db.source(file)?;
    let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new();

    let mut stack = vec![source.tree().root_node()];
    let mut cursor = source.tree().walk();
    while let Some(node) = stack.pop() {
        if raw.len() >= MAX_TOKENS {
            break;
        }
        if matches!(node.kind(), "identifier" | "type_identifier") {
            if let Some((kind, modifiers)) = classify(db, file, node) {
                let start = source.position_at(node.start_byte() as u32);
                let end = source.position_at(node.end_byte() as u32);
                // A token that spans lines cannot be encoded; none do in Java.
                if start.line == end.line {
                    raw.push((
                        start.line,
                        start.character,
                        end.character - start.character,
                        kind,
                        modifiers,
                    ));
                }
            }
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    raw.sort_by_key(|(line, column, ..)| (*line, *column));

    // The protocol encodes each token relative to the previous one.
    let mut data = Vec::with_capacity(raw.len());
    let mut previous_line = 0;
    let mut previous_start = 0;
    for (line, start, length, kind, modifiers) in raw {
        let delta_line = line - previous_line;
        let delta_start = if delta_line == 0 {
            start - previous_start
        } else {
            start
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: kind,
            token_modifiers_bitset: modifiers,
        });
        previous_line = line;
        previous_start = start;
    }
    Some(SemanticTokens {
        result_id: None,
        data,
    })
}

fn classify(db: &Db, file: FileId, node: Node<'_>) -> Option<(u32, u32)> {
    // Resolution happens at the token's own offset so scope is correct.
    let local_context = scope::context_at(db, file, node.start_byte() as u32)?;
    let target = target::target_of_node(&local_context, node)?;
    let is_declaration = target
        .origin(db)
        .and_then(|origin| origin.name_range())
        .is_some_and(|range| range.0 == node.start_byte() as u32);
    let declaration_bit = if is_declaration {
        modifier::DECLARATION
    } else {
        0
    };
    Some(match target {
        Target::Local { name, .. } => {
            let local = local_context.local(&name)?;
            let kind = if local.is_parameter {
                index::PARAMETER
            } else {
                index::VARIABLE
            };
            (kind, declaration_bit)
        }
        Target::Field { owner, name } => {
            let class = db.class(&owner)?;
            let field = class.find_field(&name)?;
            let mut modifiers = declaration_bit;
            if field.modifiers.is_static {
                modifiers |= modifier::STATIC;
            }
            if field.modifiers.is_final {
                modifiers |= modifier::READONLY;
            }
            if field.modifiers.is_deprecated {
                modifiers |= modifier::DEPRECATED;
            }
            (index::PROPERTY, modifiers)
        }
        Target::Method { owner, index, .. } => {
            let class = db.class(&owner)?;
            let method = class.methods.get(index as usize)?;
            let mut modifiers = declaration_bit;
            if method.modifiers.is_static {
                modifiers |= modifier::STATIC;
            }
            if method.modifiers.is_deprecated {
                modifiers |= modifier::DEPRECATED;
            }
            (index::METHOD, modifiers)
        }
        Target::Class(binary) => {
            let class = db.class(&binary)?;
            let kind = match class.kind {
                java_semantic::ClassKind::Interface | java_semantic::ClassKind::Annotation => {
                    index::INTERFACE
                }
                java_semantic::ClassKind::Enum => index::ENUM,
                java_semantic::ClassKind::Class => index::CLASS,
            };
            let mut modifiers = declaration_bit;
            if class.modifiers.is_deprecated {
                modifiers |= modifier::DEPRECATED;
            }
            (kind, modifiers)
        }
        Target::TypeParam { .. } => (index::TYPE_PARAMETER, declaration_bit),
        Target::Package(_) => (index::NAMESPACE, 0),
    })
}

//! Small helpers for walking a Java parse tree.
//!
//! Everything here is a thin convenience over `tree_sitter::Node`; the grammar's
//! node-kind names are used directly rather than being wrapped in a typed AST,
//! which keeps the syntax layer honest about what the parser actually produced.

use tree_sitter::{Node, Tree};

/// The smallest named node whose byte range contains `offset`.
pub fn named_node_at(tree: &Tree, offset: u32) -> Node<'_> {
    let offset = offset as usize;
    tree.root_node()
        .named_descendant_for_byte_range(offset, offset)
        .unwrap_or_else(|| tree.root_node())
}

/// The smallest node, named or anonymous, containing `offset`.
pub fn node_at(tree: &Tree, offset: u32) -> Node<'_> {
    let offset = offset as usize;
    tree.root_node()
        .descendant_for_byte_range(offset, offset)
        .unwrap_or_else(|| tree.root_node())
}

/// The identifier-like token touching `offset`, preferring the one that ends
/// there so that a cursor just after a name still resolves it.
pub fn identifier_at(tree: &Tree, offset: u32) -> Option<Node<'_>> {
    let at = node_at(tree, offset);
    if is_name_token(&at) && at.start_byte() as u32 <= offset {
        return Some(at);
    }
    if offset > 0 {
        let before = node_at(tree, offset - 1);
        if is_name_token(&before) {
            return Some(before);
        }
    }
    None
}

fn is_name_token(node: &Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "type_identifier" | "this" | "super"
    )
}

/// Walks from `node` up through its parents, `node` itself first.
pub fn ancestors<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    std::iter::successors(Some(node), |n| n.parent())
}

/// The nearest ancestor (including `node`) with one of the given kinds.
pub fn ancestor_of_kind<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    ancestors(node).find(|n| kinds.contains(&n.kind()))
}

/// Named children of `node`, skipping punctuation and comments.
pub fn named_children<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node
        .named_children(&mut cursor)
        .filter(|c| !matches!(c.kind(), "line_comment" | "block_comment" | "comment"))
        .collect();
    children.into_iter()
}

/// All named children, including comments.
pub fn all_named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub fn field<'t>(node: Node<'t>, name: &str) -> Option<Node<'t>> {
    node.child_by_field_name(name)
}

pub fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

/// The `name` field of a declaration, as source text.
pub fn name_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    field(node, "name").map(|n| text(n, source))
}

/// Renders a subtree as an s-expression with byte ranges; used by the tree
/// dumping tool and by test failure messages.
pub fn debug_tree(node: Node<'_>, source: &str) -> String {
    let mut out = String::new();
    write_tree(node, source, 0, &mut out);
    out
}

fn write_tree(node: Node<'_>, source: &str, depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(node.kind());
    if node.child_count() == 0 {
        out.push_str(" '");
        out.push_str(text(node, source));
        out.push('\'');
    }
    if node.is_missing() {
        out.push_str(" [MISSING]");
    }
    if node.is_error() {
        out.push_str(" [ERROR]");
    }
    out.push('\n');
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        write_tree(child, source, depth + 1, out);
    }
}

/// Node kinds that denote a Java expression.
///
/// Used to decide how far out from a cursor an expression extends, which both
/// hover and completion need: the token under the cursor is rarely the whole
/// expression the user means.
pub fn is_expression(kind: &str) -> bool {
    matches!(
        kind,
        "assignment_expression"
            | "binary_expression"
            | "instanceof_expression"
            | "lambda_expression"
            | "ternary_expression"
            | "update_expression"
            | "unary_expression"
            | "cast_expression"
            | "switch_expression"
            | "object_creation_expression"
            | "array_creation_expression"
            | "array_initializer"
            | "field_access"
            | "array_access"
            | "method_invocation"
            | "method_reference"
            | "parenthesized_expression"
            | "class_literal"
            | "identifier"
            // Error recovery reinterprets a half-typed `receiver.` as a type
            // name, so a type identifier can stand where an expression does.
            | "type_identifier"
            | "this"
            | "super"
            | "scoped_identifier"
            | "scoped_type_identifier"
            | "decimal_integer_literal"
            | "hex_integer_literal"
            | "octal_integer_literal"
            | "binary_integer_literal"
            | "decimal_floating_point_literal"
            | "hex_floating_point_literal"
            | "character_literal"
            | "string_literal"
            | "text_block"
            | "true"
            | "false"
            | "null_literal"
    )
}

/// The widest expression that ends at `offset`, starting from the token there.
///
/// `items.get(0)` with the cursor after the `)` yields the whole invocation,
/// not the closing parenthesis.
pub fn expression_ending_at(tree: &Tree, offset: u32) -> Option<Node<'_>> {
    let start = named_node_at(tree, offset.saturating_sub(1));
    let mut best = is_expression(start.kind()).then_some(start);
    for ancestor in ancestors(start) {
        if ancestor.end_byte() as u32 > offset {
            break;
        }
        // A lambda's body runs to the end of the expression, so a lambda will
        // appear to end here too — but a lambda cannot be the qualifier of a
        // member access, so climbing past one would mistake the lambda's own
        // target type for the receiver.
        if matches!(ancestor.kind(), "lambda_expression" | "method_reference") {
            break;
        }
        if is_expression(ancestor.kind()) {
            best = Some(ancestor);
        }
    }
    best
}

/// The outer-instance expression of a qualified creation, `outer` in `outer.new Inner()`.
pub fn creation_qualifier<'t>(creation: Node<'t>) -> Option<Node<'t>> {
    let type_node = field(creation, "type")?;
    named_children(creation)
        .find(|child| child.end_byte() <= type_node.start_byte() && is_expression(child.kind()))
}

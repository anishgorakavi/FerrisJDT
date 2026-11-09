//! Per-file outlines: the package, the imports, and every declared class.
//!
//! An outline is what other files need in order to resolve names, and it is
//! deliberately cheap: no member signatures, no type resolution. Editing inside
//! a method body leaves the outline unchanged, which is what keeps a keystroke
//! from invalidating the rest of the workspace.

use crate::name::{self, Name};
use crate::symbols::{ClassKind, FileId};
use java_syntax::{ast, Node, SourceFile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    /// Dotted path as written, without a trailing `*`.
    pub path: Name,
    pub is_static: bool,
    pub is_wildcard: bool,
    pub range: (u32, u32),
}

impl ImportDecl {
    /// Last segment of a single-type import: the simple name it introduces.
    pub fn simple_name(&self) -> &str {
        name::simple_name(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineClass {
    /// Binary name, with `$` separating nested classes. An anonymous class is
    /// numbered the way the compiler numbers it: `Outer$1`.
    pub binary: Name,
    /// Display name. For an anonymous class this is the type it implements,
    /// which is what a reader recognises it by.
    pub simple: Name,
    pub kind: ClassKind,
    /// Byte range of the whole declaration.
    pub range: (u32, u32),
    /// Byte range of just the declared name.
    pub name_range: (u32, u32),
    /// Index into `FileOutline::classes` of the enclosing class.
    pub parent: Option<u32>,
    pub is_static: bool,
    /// True for a class declared by `new Interface() { ... }`, which has no
    /// name of its own and cannot be referred to.
    pub is_anonymous: bool,
    /// For an anonymous class, the type it extends or implements, as written.
    pub anonymous_base: Option<Name>,
    /// The nested builder class Lombok generates for this class, when it asks for one.
    pub builder_class: Option<BuilderDecl>,
}

/// A builder class implied by `@Builder` or `@SuperBuilder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderDecl {
    pub name: Name,
    /// `@SuperBuilder` also generates an `Impl` subclass.
    pub is_super: bool,
}

#[derive(Debug, Clone)]
pub struct FileOutline {
    pub file: FileId,
    pub package: Option<Name>,
    pub imports: Vec<ImportDecl>,
    /// Every class in the file, outermost first, in declaration order.
    pub classes: Vec<OutlineClass>,
}

impl FileOutline {
    /// Whether two outlines agree on everything other files can observe.
    /// Edits inside a method body leave this unchanged.
    pub fn same_shape(&self, other: &FileOutline) -> bool {
        self.package == other.package
            && self.imports == other.imports
            && self.classes.len() == other.classes.len()
            && self.classes.iter().zip(&other.classes).all(|(a, b)| {
                a.binary == b.binary
                    && a.kind == b.kind
                    && a.parent == b.parent
                    && a.builder_class == b.builder_class
            })
    }

    /// Classes Lombok generates, as (binary name, simple name, index of the enclosing class).
    pub fn synthetic_classes(&self) -> Vec<(Name, Name, u32)> {
        let mut found = Vec::new();
        for (index, class) in self.classes.iter().enumerate() {
            let Some(builder) = &class.builder_class else {
                continue;
            };
            let binary = name::name(&format!("{}${}", class.binary, builder.name));
            if builder.is_super {
                let implementation = format!("{}Impl", builder.name);
                found.push((
                    name::name(&format!("{binary}Impl")),
                    name::name(&implementation),
                    index as u32,
                ));
            }
            found.push((binary, builder.name.clone(), index as u32));
        }
        found
    }

    /// Top-level classes, which are the ones a package exposes.
    pub fn top_level(&self) -> impl Iterator<Item = &OutlineClass> {
        self.classes.iter().filter(|c| c.parent.is_none())
    }

    pub fn find(&self, binary: &str) -> Option<&OutlineClass> {
        self.classes.iter().find(|c| &*c.binary == binary)
    }

    /// The innermost class whose range contains `offset`.
    pub fn class_at(&self, offset: u32) -> Option<&OutlineClass> {
        self.classes
            .iter()
            .filter(|c| c.range.0 <= offset && offset <= c.range.1)
            .min_by_key(|c| c.range.1 - c.range.0)
    }
}

const DECLARATION_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "annotation_type_declaration",
    "record_declaration",
];

pub fn kind_of(node_kind: &str) -> Option<ClassKind> {
    Some(match node_kind {
        "class_declaration" | "record_declaration" => ClassKind::Class,
        "interface_declaration" => ClassKind::Interface,
        "enum_declaration" => ClassKind::Enum,
        "annotation_type_declaration" => ClassKind::Annotation,
        _ => return None,
    })
}

/// Computes the outline of a parsed file.
pub fn outline(file: FileId, source: &SourceFile) -> FileOutline {
    let text = source.text();
    let root = source.tree().root_node();
    let mut package = None;
    let mut imports = Vec::new();
    let mut classes = Vec::new();

    for child in ast::named_children(root) {
        match child.kind() {
            "package_declaration" => {
                package = ast::named_children(child)
                    .find(|n| matches!(n.kind(), "identifier" | "scoped_identifier"))
                    .map(|n| name::name(&dotted(n, text)));
            }
            "import_declaration" => {
                if let Some(import) = lower_import(child, text) {
                    imports.push(import);
                }
            }
            kind if DECLARATION_KINDS.contains(&kind) => {
                // Anonymous classes are numbered per top-level class, as the
                // compiler does, so the counter starts fresh here.
                let mut anonymous = 0u32;
                collect(
                    child,
                    text,
                    package.as_deref(),
                    None,
                    &mut classes,
                    &mut anonymous,
                );
            }
            _ => {}
        }
    }
    // Builders exist only where Lombok is imported, and a class that already declares a nested
    // class of that name has written its own.
    let lombok = imports
        .iter()
        .any(|i| &*i.path == "lombok" || i.path.starts_with("lombok."));
    let declared: Vec<(Option<u32>, Name)> = classes
        .iter()
        .map(|c| (c.parent, c.simple.clone()))
        .collect();
    for (index, class) in classes.iter_mut().enumerate() {
        let clashes = class.builder_class.as_ref().is_some_and(|builder| {
            declared
                .iter()
                .any(|(parent, simple)| *parent == Some(index as u32) && *simple == builder.name)
        });
        if !lombok || clashes {
            class.builder_class = None;
        }
    }
    FileOutline {
        file,
        package,
        imports,
        classes,
    }
}

fn lower_import(node: Node<'_>, text: &str) -> Option<ImportDecl> {
    let mut is_static = false;
    let mut is_wildcard = false;
    let mut path = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "static" => is_static = true,
            "asterisk" => is_wildcard = true,
            "identifier" | "scoped_identifier" => path = Some(dotted(child, text)),
            _ => {}
        }
    }
    Some(ImportDecl {
        path: name::name(&path?),
        is_static,
        is_wildcard,
        range: (node.start_byte() as u32, node.end_byte() as u32),
    })
}

/// Reconstructs a dotted name from a `scoped_identifier` or `identifier`,
/// dropping any whitespace or comments between the segments.
pub fn dotted(node: Node<'_>, text: &str) -> String {
    match node.kind() {
        "identifier" | "type_identifier" => ast::text(node, text).to_string(),
        _ => {
            let mut parts = Vec::new();
            collect_segments(node, text, &mut parts);
            parts.join(".")
        }
    }
}

fn collect_segments(node: Node<'_>, text: &str, out: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "type_identifier" => out.push(ast::text(node, text).to_string()),
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_segments(child, text, out);
            }
        }
        // `generic_type` inside a scoped name: keep only the name part.
        "generic_type" => {
            if let Some(first) = node.named_child(0) {
                collect_segments(first, text, out);
            }
        }
        _ => {}
    }
}

fn collect(
    node: Node<'_>,
    text: &str,
    package: Option<&str>,
    parent: Option<u32>,
    out: &mut Vec<OutlineClass>,
    anonymous_counter: &mut u32,
) {
    let Some(kind) = kind_of(node.kind()) else {
        return;
    };
    let Some(name_node) = ast::field(node, "name") else {
        return;
    };
    let simple = ast::text(name_node, text);
    if simple.is_empty() {
        return;
    }
    let binary = binary_name(simple, package, parent, out);
    let index = out.len() as u32;
    out.push(OutlineClass {
        binary: name::name(&binary),
        simple: name::name(simple),
        kind,
        range: (node.start_byte() as u32, node.end_byte() as u32),
        name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
        parent,
        is_static: has_modifier(node, text, "static")
            || parent.is_none()
            || kind != ClassKind::Class
            || node.kind() == "record_declaration",
        is_anonymous: false,
        anonymous_base: None,
        builder_class: if node.kind() == "class_declaration" {
            crate::lombok::builder_decl(node, text, simple).map(|(name, is_super)| BuilderDecl {
                name: name::name(&name),
                is_super,
            })
        } else {
            None
        },
    });

    let Some(body) = ast::field(node, "body") else {
        return;
    };
    collect_in_subtree(body, text, package, index, out, anonymous_counter);
}

/// Collects every class declared anywhere inside a class body.
///
/// Member classes, classes declared inside a method body, and anonymous
/// classes are all found by the same walk, because all three are nested
/// classes as far as resolution is concerned and all three need to be in the
/// outline for their members to be visible.
fn collect_in_subtree(
    body: Node<'_>,
    text: &str,
    package: Option<&str>,
    owner: u32,
    out: &mut Vec<OutlineClass>,
    anonymous_counter: &mut u32,
) {
    let mut stack: Vec<Node<'_>> = Vec::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        stack.push(child);
    }
    // Children are pushed in order and popped in reverse, so the stack is
    // reversed up front to keep declaration order stable.
    stack.reverse();
    while let Some(node) = stack.pop() {
        if DECLARATION_KINDS.contains(&node.kind()) {
            // The recursive call walks this declaration's own body.
            collect(node, text, package, Some(owner), out, anonymous_counter);
            continue;
        }
        if node.kind() == "object_creation_expression" {
            if let Some(class_body) = ast::named_children(node).find(|c| c.kind() == "class_body") {
                collect_anonymous(
                    node,
                    class_body,
                    text,
                    package,
                    owner,
                    out,
                    anonymous_counter,
                );
                // The arguments of the creation expression may themselves
                // contain classes, so they are still walked.
                let mut arguments = node.walk();
                let children: Vec<Node<'_>> = node
                    .named_children(&mut arguments)
                    .filter(|c| c.id() != class_body.id())
                    .collect();
                for child in children.into_iter().rev() {
                    stack.push(child);
                }
                continue;
            }
        }
        let mut children = node.walk();
        let collected: Vec<Node<'_>> = node.named_children(&mut children).collect();
        for child in collected.into_iter().rev() {
            stack.push(child);
        }
    }
}

fn collect_anonymous(
    creation: Node<'_>,
    class_body: Node<'_>,
    text: &str,
    package: Option<&str>,
    owner: u32,
    out: &mut Vec<OutlineClass>,
    anonymous_counter: &mut u32,
) {
    *anonymous_counter += 1;
    // The compiler names these after the top-level class: `LinkedList$1`.
    let top_level = {
        let mut at = owner;
        loop {
            match out[at as usize].parent {
                Some(parent) => at = parent,
                None => break out[at as usize].binary.clone(),
            }
        }
    };
    let binary = format!("{top_level}${anonymous_counter}");
    let base = ast::field(creation, "type").map(|ty| dotted(ty, text));
    let index = out.len() as u32;
    out.push(OutlineClass {
        binary: name::name(&binary),
        simple: name::name(
            base.as_deref()
                .map(name::simple_name)
                .unwrap_or("<anonymous>"),
        ),
        kind: ClassKind::Class,
        range: (creation.start_byte() as u32, creation.end_byte() as u32),
        // There is no name to select, so the created type stands in for it.
        name_range: ast::field(creation, "type")
            .map(|ty| (ty.start_byte() as u32, ty.end_byte() as u32))
            .unwrap_or((
                creation.start_byte() as u32,
                creation.start_byte() as u32 + 3,
            )),
        parent: Some(owner),
        is_static: false,
        is_anonymous: true,
        anonymous_base: base.as_deref().map(name::name),
        builder_class: None,
    });
    collect_in_subtree(class_body, text, package, index, out, anonymous_counter);
}

/// Binary name of a class, given its enclosing class or package.
fn binary_name(
    simple: &str,
    package: Option<&str>,
    parent: Option<u32>,
    out: &[OutlineClass],
) -> String {
    match parent {
        Some(index) => format!("{}${simple}", out[index as usize].binary),
        None => match package {
            Some(package) if !package.is_empty() => format!("{package}.{simple}"),
            _ => simple.to_string(),
        },
    }
}

pub fn has_modifier(node: Node<'_>, text: &str, keyword: &str) -> bool {
    let Some(modifiers) = ast::named_children(node).find(|n| n.kind() == "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .any(|m| ast::text(m, text) == keyword);
    found
}

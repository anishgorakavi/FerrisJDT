//! Turning a type name in source into a resolved type.
//!
//! Java's rules for what a simple name means are order-sensitive (JLS 6.5.5):
//! a type parameter shadows a nested class, which shadows an import, which
//! shadows the package, and so on. That order is implemented explicitly in
//! `resolve_class_name` rather than being approximated, because getting it
//! wrong is how a server ends up completing members of the wrong type.

use crate::db::Db;
use crate::name::{self, Name};
use crate::outline::{self, FileOutline};
use crate::symbols::ClassRef;
use crate::types::{Type, TypeVar, WildcardKind};
use java_syntax::{ast, Node};

/// What is in scope for resolving a name at some point in a file.
pub struct Scope<'a> {
    pub db: &'a Db,
    pub outline: &'a FileOutline,
    /// Binary names of enclosing classes, innermost first.
    pub enclosing: Vec<Name>,
    /// Type parameters in scope with their bounds, innermost last.
    pub type_vars: Vec<(Name, Vec<Type>)>,
}

impl<'a> Scope<'a> {
    pub fn new(db: &'a Db, outline: &'a FileOutline) -> Scope<'a> {
        Scope {
            db,
            outline,
            enclosing: Vec::new(),
            type_vars: Vec::new(),
        }
    }

    pub fn type_var(&self, name: &str) -> Option<&(Name, Vec<Type>)> {
        self.type_vars.iter().rev().find(|(n, _)| &**n == name)
    }

    fn package(&self) -> &str {
        self.outline.package.as_deref().unwrap_or("")
    }

    /// Resolves a type name as written in source to a binary class name.
    pub fn resolve_class_name(&self, written: &str) -> Option<Name> {
        if written.is_empty() {
            return None;
        }
        if let Some(dot) = written.find('.') {
            return self.resolve_qualified(written, dot);
        }
        self.resolve_simple(written)
    }

    fn resolve_simple(&self, simple: &str) -> Option<Name> {
        // 1. A nested class of an enclosing class, or one it inherits.
        for enclosing in &self.enclosing {
            if let Some(found) = self.nested_in(enclosing, simple, 0) {
                return Some(found);
            }
        }
        // 2. A single-type import.
        for import in &self.outline.imports {
            if import.is_wildcard || import.is_static {
                continue;
            }
            if import.simple_name() == simple {
                if let Some(found) = self.existing_binary(&import.path) {
                    return Some(found);
                }
            }
        }
        // 3. A top-level class in this file, then anything else in the package.
        if let Some(class) = self.outline.top_level().find(|c| &*c.simple == simple) {
            return Some(class.binary.clone());
        }
        let package = self.package();
        if let Some(found) = self.existing_binary(&qualify(package, simple)) {
            return Some(found);
        }
        // 4. Static imports can introduce a nested type.
        for import in &self.outline.imports {
            if import.is_static && !import.is_wildcard && import.simple_name() == simple {
                if let Some(found) = self.existing_binary(&import.path) {
                    return Some(found);
                }
            }
        }
        // 5. On-demand imports, then java.lang, then the default package.
        for import in &self.outline.imports {
            if !import.is_wildcard {
                continue;
            }
            let candidate = format!("{}.{simple}", import.path);
            if let Some(found) = self.existing_binary(&candidate) {
                return Some(found);
            }
            // `import static java.util.Arrays.*` can also bring in a nested type.
            if import.is_static {
                if let Some(found) = self.nested_in(&import.path, simple, 0) {
                    return Some(found);
                }
            }
        }
        if let Some(found) = self.existing_binary(&format!("java.lang.{simple}")) {
            return Some(found);
        }
        if !package.is_empty() {
            // A class in the default package, which course projects often use.
            if let Some(found) = self.existing_binary(simple) {
                return Some(found);
            }
        }
        None
    }

    /// Resolves `a.b.C`, which may be a fully qualified class, a nested class
    /// spelled with dots, or a simple name followed by nested classes.
    fn resolve_qualified(&self, written: &str, first_dot: usize) -> Option<Name> {
        if let Some(found) = self.existing_binary(written) {
            return Some(found);
        }
        // Resolve the leading simple name, then append the rest as nesting.
        let head = &written[..first_dot];
        let rest = &written[first_dot + 1..];
        let base = self.resolve_simple(head)?;
        let mut candidate = base.to_string();
        for segment in rest.split('.') {
            candidate.push('$');
            candidate.push_str(segment);
        }
        self.existing_binary(&candidate)
    }

    /// Accepts a dotted name that may denote a nested class, trying `$` in
    /// place of each trailing `.` from the right.
    fn existing_binary(&self, candidate: &str) -> Option<Name> {
        if self.db.has_class(candidate) {
            return Some(name::name(candidate));
        }
        let bytes = candidate.as_bytes();
        let mut rewritten = candidate.to_string();
        for (index, byte) in bytes.iter().enumerate().rev() {
            if *byte == b'.' {
                // Safe: ASCII '.' is one byte and lies on a char boundary.
                rewritten.replace_range(index..index + 1, "$");
                if self.db.has_class(&rewritten) {
                    return Some(name::name(&rewritten));
                }
            }
        }
        None
    }

    /// Looks for a nested class `simple` inside `owner` or its supertypes.
    fn nested_in(&self, owner: &str, simple: &str, depth: u32) -> Option<Name> {
        if depth > crate::MAX_HIERARCHY_DEPTH {
            return None;
        }
        let direct = format!("{owner}${simple}");
        if self.db.has_class(&direct) {
            return Some(name::name(&direct));
        }
        // An enclosing class's own enclosing classes are also in scope.
        if let Some(parent) = name::enclosing_class(owner) {
            if let Some(found) = self.nested_in(parent, simple, depth + 1) {
                return Some(found);
            }
        }
        let Some(class) = self.db.class(owner) else {
            return self.nested_in_written_supertypes(owner, simple, depth);
        };
        for supertype in class.superclass.iter().chain(&class.interfaces) {
            if let Some(name) = supertype.class_name() {
                if let Some(found) = self.nested_in(name, simple, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// The fallback for a class of this file that is still being lowered: its supertypes are read as
    /// written and resolved from the scope around it.
    fn nested_in_written_supertypes(&self, owner: &str, simple: &str, depth: u32) -> Option<Name> {
        let declared = self.outline.find(owner)?;
        let mut outer = Vec::new();
        let mut parent = declared.parent;
        while let Some(at) = parent {
            let class = &self.outline.classes[at as usize];
            outer.push(class.binary.clone());
            parent = class.parent;
        }
        let around = Scope {
            db: self.db,
            outline: self.outline,
            enclosing: outer,
            type_vars: Vec::new(),
        };
        for written in &declared.supertypes {
            if let Some(resolved) = around.resolve_class_name(written) {
                if let Some(found) = self.nested_in(&resolved, simple, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Resolves a type as written at a syntax node.
    ///
    /// An unresolvable name becomes `Type::Unknown` rather than a guess, so
    /// later stages can stay silent instead of reporting something false.
    pub fn lower_type(&self, node: Node<'_>, text: &str) -> Type {
        match node.kind() {
            "integral_type" | "floating_point_type" => {
                let keyword = ast::text(node, text).trim();
                crate::types::Primitive::parse(keyword)
                    .map(Type::Primitive)
                    .unwrap_or(Type::Unknown)
            }
            "boolean_type" => Type::BOOLEAN,
            "void_type" => Type::VOID,
            "array_type" => {
                let element = ast::field(node, "element")
                    .map(|e| self.lower_type(e, text))
                    .unwrap_or(Type::Unknown);
                let depth = ast::field(node, "dimensions")
                    .map(|d| ast::text(d, text).matches('[').count())
                    .unwrap_or(1)
                    .max(1);
                (0..depth).fold(element, |inner, _| Type::Array(Box::new(inner)))
            }
            "generic_type" => {
                let mut children = ast::named_children(node);
                let base = match children.next() {
                    Some(base) => base,
                    None => return Type::Unknown,
                };
                let written = outline::dotted(base, text);
                let args: Vec<Type> = children
                    .find(|c| c.kind() == "type_arguments")
                    .map(|list| {
                        ast::named_children(list)
                            .map(|arg| self.lower_type(arg, text))
                            .collect()
                    })
                    .unwrap_or_default();
                self.class_type(&written, args)
            }
            "wildcard" => {
                let mut kind = WildcardKind::Unbounded;
                let mut bound = None;
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "extends" => kind = WildcardKind::Extends,
                        "super" => kind = WildcardKind::Super,
                        _ if child.is_named() && child.kind() != "annotation" => {
                            bound = Some(Box::new(self.lower_type(child, text)));
                        }
                        _ => {}
                    }
                }
                Type::Wildcard { kind, bound }
            }
            "annotated_type" => ast::named_children(node)
                .filter(|c| !c.kind().ends_with("annotation"))
                .last()
                .map(|inner| self.lower_type(inner, text))
                .unwrap_or(Type::Unknown),
            "type_identifier" | "scoped_type_identifier" | "identifier" | "scoped_identifier" => {
                let written = outline::dotted(node, text);
                self.class_type(&written, Vec::new())
            }
            // `var` is Java 10; at source level 8 it is an ordinary type name
            // that will not resolve, which is the correct outcome.
            _ => Type::Unknown,
        }
    }

    /// The class that declares type variable `name`: the innermost enclosing class with a type
    /// parameter of that name, since an inner class's use of its outer class's `K` is the same
    /// variable. A name no class declares is a method's own, attributed to the innermost class.
    pub fn type_var_owner(&self, name: &str) -> Option<Name> {
        self.enclosing
            .iter()
            .find(|class| {
                self.outline
                    .find(class)
                    .is_some_and(|declared| declared.type_params.iter().any(|p| &**p == name))
            })
            .or_else(|| self.enclosing.first())
            .cloned()
    }

    /// Builds a type from a written name and already-lowered type arguments.
    pub fn class_type(&self, written: &str, args: Vec<Type>) -> Type {
        if !written.contains('.') {
            if let Some((name, bounds)) = self.type_var(written) {
                return Type::Var(Box::new(TypeVar::new(
                    name.clone(),
                    self.type_var_owner(name),
                    bounds.clone(),
                )));
            }
        }
        match self.resolve_class_name(written) {
            Some(binary) => Type::Class { name: binary, args },
            None => Type::Unknown,
        }
    }

    /// Resolves a name that may be a class or a package, for qualified
    /// expressions such as `java.util.Arrays.asList(...)`.
    pub fn resolve_class(&self, written: &str) -> Option<ClassRef> {
        self.db.class(&self.resolve_class_name(written)?)
    }
}

pub fn qualify(package: &str, simple: &str) -> String {
    if package.is_empty() {
        simple.to_string()
    } else {
        format!("{package}.{simple}")
    }
}

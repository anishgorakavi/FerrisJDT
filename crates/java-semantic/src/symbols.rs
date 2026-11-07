//! Declarations, as the resolver sees them.
//!
//! Source classes and classpath classes are lowered into the same `ClassData`
//! so that every later stage — member lookup, completion, hover — treats a
//! student's `BSTNode` and `java.util.ArrayList` identically.

use crate::name::Name;
use crate::types::{Substitution, Type};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    /// No modifier: visible within the package.
    Package,
    Private,
}

impl Visibility {
    pub fn as_keyword(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Protected => "protected",
            Visibility::Package => "",
            Visibility::Private => "private",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub is_static: bool,
    pub is_final: bool,
    pub is_abstract: bool,
    pub is_synchronized: bool,
    pub is_native: bool,
    pub is_transient: bool,
    pub is_volatile: bool,
    pub is_default: bool,
    pub is_varargs: bool,
    pub is_deprecated: bool,
    /// Compiler-generated: bridge methods, `this$0` fields, lambda bodies.
    pub is_synthetic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    Class,
    Interface,
    Enum,
    Annotation,
}

impl ClassKind {
    pub fn keyword(self) -> &'static str {
        match self {
            ClassKind::Class => "class",
            ClassKind::Interface => "interface",
            ClassKind::Enum => "enum",
            ClassKind::Annotation => "@interface",
        }
    }
}

/// Where a declaration came from, which is what go-to-definition needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A file in the workspace. Ranges are byte offsets into that file.
    Source {
        file: FileId,
        /// Full declaration, used as the definition range.
        range: (u32, u32),
        /// Just the declared name, used as the selection range.
        name_range: (u32, u32),
    },
    /// A class file on the classpath. There is no source range; navigation
    /// reports the owning archive instead.
    Classpath { archive: Arc<str> },
    /// Synthesised by the server, such as an implicit default constructor.
    Synthetic,
}

impl Origin {
    pub fn file(&self) -> Option<FileId> {
        match self {
            Origin::Source { file, .. } => Some(*file),
            _ => None,
        }
    }

    pub fn range(&self) -> Option<(u32, u32)> {
        match self {
            Origin::Source { range, .. } => Some(*range),
            _ => None,
        }
    }

    pub fn name_range(&self) -> Option<(u32, u32)> {
        match self {
            Origin::Source { name_range, .. } => Some(*name_range),
            _ => None,
        }
    }
}

/// Identifies an indexed file. Stable for the lifetime of the server: ids are
/// handed out once per path and are never reused, so cached references stay
/// valid across edits to the file's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

#[derive(Debug, Clone)]
pub struct TypeParam {
    pub name: Name,
    pub bounds: Vec<Type>,
}

#[derive(Debug, Clone)]
pub struct FieldData {
    pub name: Name,
    pub ty: Type,
    pub visibility: Visibility,
    pub modifiers: Modifiers,
    pub origin: Origin,
    pub doc: Option<Arc<str>>,
    /// Source text of the initialiser, shown on hover for constants.
    pub constant_value: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct ParamData {
    pub name: Name,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct MethodData {
    pub name: Name,
    pub type_params: Vec<TypeParam>,
    pub params: Vec<ParamData>,
    pub ret: Type,
    pub throws: Vec<Type>,
    pub visibility: Visibility,
    pub modifiers: Modifiers,
    pub origin: Origin,
    pub doc: Option<Arc<str>>,
    pub is_constructor: bool,
}

impl MethodData {
    /// Signature used to decide whether one method overrides another: the name
    /// plus erased parameter types (JLS 8.4.2).
    pub fn erased_signature(&self) -> String {
        let mut out = String::with_capacity(self.name.len() + 8 * self.params.len());
        out.push_str(&self.name);
        out.push('(');
        for (index, param) in self.params.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&param.ty.erase().display_full());
        }
        out.push(')');
        out
    }

    /// `name(Type a, Type b)` for display in completion and signature help.
    pub fn display_signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                let mut ty = param.ty.display_short();
                // A varargs method's last parameter is declared as an array.
                if self.modifiers.is_varargs && index + 1 == self.params.len() {
                    if let Some(stripped) = ty.strip_suffix("[]") {
                        ty = format!("{stripped}...");
                    }
                }
                format!("{ty} {}", param.name)
            })
            .collect();
        format!("{}({})", self.name, params.join(", "))
    }

    pub fn accepts_arity(&self, count: usize) -> bool {
        if self.modifiers.is_varargs {
            count + 1 >= self.params.len()
        } else {
            count == self.params.len()
        }
    }
}

/// A class, interface, enum, or annotation declaration.
#[derive(Debug, Clone)]
pub struct ClassData {
    /// Binary name: `ds.BinarySearchTree`, `java.util.Map$Entry`.
    pub name: Name,
    pub kind: ClassKind,
    pub visibility: Visibility,
    pub modifiers: Modifiers,
    pub type_params: Vec<TypeParam>,
    pub superclass: Option<Type>,
    pub interfaces: Vec<Type>,
    pub fields: Vec<FieldData>,
    pub methods: Vec<MethodData>,
    /// Binary names of directly nested classes.
    pub nested: Vec<Name>,
    pub origin: Origin,
    pub doc: Option<Arc<str>>,
}

impl ClassData {
    pub fn simple_name(&self) -> &str {
        crate::name::simple_name(&self.name)
    }

    pub fn package(&self) -> &str {
        crate::name::package_of(&self.name)
    }

    pub fn is_interface(&self) -> bool {
        self.kind == ClassKind::Interface
    }

    /// Maps this class's type parameters onto the given type arguments.
    ///
    /// A raw use of a generic class — `List x` rather than `List<String> x` —
    /// substitutes each variable with its erasure, which is how a raw receiver
    /// still yields usable members instead of unresolved variables.
    pub fn substitution_for(&self, args: &[Type]) -> Substitution {
        let mut subst = Substitution::new();
        if self.type_params.is_empty() {
            return subst;
        }
        if args.len() == self.type_params.len() {
            for (param, arg) in self.type_params.iter().zip(args) {
                subst.insert(param.name.clone(), arg.clone());
            }
        } else {
            for param in &self.type_params {
                let erasure = param
                    .bounds
                    .first()
                    .map(Type::erase)
                    .unwrap_or_else(Type::object);
                subst.insert(param.name.clone(), erasure);
            }
        }
        subst
    }

    /// This class as a type with its own parameters applied, which is the type
    /// of `this` inside its body.
    pub fn self_type(&self) -> Type {
        Type::Class {
            name: self.name.clone(),
            args: self
                .type_params
                .iter()
                .map(|p| {
                    Type::Var(Box::new(crate::types::TypeVar::new(
                        p.name.clone(),
                        Some(self.name.clone()),
                        p.bounds.clone(),
                    )))
                })
                .collect(),
        }
    }

    pub fn find_field(&self, name: &str) -> Option<&FieldData> {
        self.fields.iter().find(|f| &*f.name == name)
    }

    /// Methods with a given name, in declaration order.
    ///
    /// Returns a `Vec` rather than an iterator so the queried name is borrowed
    /// only for the call; an iterator would tie its lifetime to `&self` and
    /// force callers to keep the class alive longer than they need to.
    pub fn methods_named(&self, name: &str) -> Vec<&MethodData> {
        self.methods.iter().filter(|m| &*m.name == name).collect()
    }

    pub fn constructors(&self) -> impl Iterator<Item = &MethodData> {
        self.methods.iter().filter(|m| m.is_constructor)
    }
}

pub type ClassRef = Arc<ClassData>;

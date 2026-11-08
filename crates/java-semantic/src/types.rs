//! The type model.
//!
//! A `Type` is a plain owned tree: class types name their class rather than
//! pointing at it, which keeps types cheap to clone and — crucially — makes
//! recursive and F-bounded declarations (`class Node { Node next; }`,
//! `<E extends Comparable<? super E>>`) representable without reference
//! cycles. Looking a name up is the database's job, not the type's.

use crate::name::Name;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    Boolean,
    Byte,
    Short,
    Char,
    Int,
    Long,
    Float,
    Double,
    Void,
}

impl Primitive {
    pub fn as_str(self) -> &'static str {
        match self {
            Primitive::Boolean => "boolean",
            Primitive::Byte => "byte",
            Primitive::Short => "short",
            Primitive::Char => "char",
            Primitive::Int => "int",
            Primitive::Long => "long",
            Primitive::Float => "float",
            Primitive::Double => "double",
            Primitive::Void => "void",
        }
    }

    pub fn parse(text: &str) -> Option<Primitive> {
        Some(match text {
            "boolean" => Primitive::Boolean,
            "byte" => Primitive::Byte,
            "short" => Primitive::Short,
            "char" => Primitive::Char,
            "int" => Primitive::Int,
            "long" => Primitive::Long,
            "float" => Primitive::Float,
            "double" => Primitive::Double,
            "void" => Primitive::Void,
            _ => return None,
        })
    }

    /// The wrapper class a value of this type boxes to (JLS 5.1.7).
    pub fn boxed(self) -> &'static str {
        match self {
            Primitive::Boolean => "java.lang.Boolean",
            Primitive::Byte => "java.lang.Byte",
            Primitive::Short => "java.lang.Short",
            Primitive::Char => "java.lang.Character",
            Primitive::Int => "java.lang.Integer",
            Primitive::Long => "java.lang.Long",
            Primitive::Float => "java.lang.Float",
            Primitive::Double => "java.lang.Double",
            Primitive::Void => "java.lang.Void",
        }
    }

    /// The primitive a wrapper class unboxes to, if it is one.
    pub fn unboxed(class: &str) -> Option<Primitive> {
        Some(match class {
            "java.lang.Boolean" => Primitive::Boolean,
            "java.lang.Byte" => Primitive::Byte,
            "java.lang.Short" => Primitive::Short,
            "java.lang.Character" => Primitive::Char,
            "java.lang.Integer" => Primitive::Int,
            "java.lang.Long" => Primitive::Long,
            "java.lang.Float" => Primitive::Float,
            "java.lang.Double" => Primitive::Double,
            _ => return None,
        })
    }

    pub fn is_numeric(self) -> bool {
        !matches!(self, Primitive::Boolean | Primitive::Void)
    }

    pub fn is_integral(self) -> bool {
        matches!(
            self,
            Primitive::Byte | Primitive::Short | Primitive::Char | Primitive::Int | Primitive::Long
        )
    }

    /// Rank used for widening primitive conversion (JLS 5.1.2); a smaller rank
    /// widens to a larger one without a cast.
    fn widening_rank(self) -> Option<u8> {
        Some(match self {
            Primitive::Byte => 1,
            Primitive::Short | Primitive::Char => 2,
            Primitive::Int => 3,
            Primitive::Long => 4,
            Primitive::Float => 5,
            Primitive::Double => 6,
            _ => return None,
        })
    }

    /// Whether `self` widens to `target` without a cast. `char` and `short` are
    /// both rank 2 but do not convert to each other.
    pub fn widens_to(self, target: Primitive) -> bool {
        if self == target {
            return true;
        }
        if (self == Primitive::Char && target == Primitive::Short)
            || (self == Primitive::Short && target == Primitive::Char)
        {
            return false;
        }
        match (self.widening_rank(), target.widening_rank()) {
            (Some(from), Some(to)) => from < to,
            _ => false,
        }
    }

    /// Result of binary numeric promotion (JLS 5.6.2).
    pub fn promote(self, other: Primitive) -> Primitive {
        for candidate in [Primitive::Double, Primitive::Float, Primitive::Long] {
            if self == candidate || other == candidate {
                return candidate;
            }
        }
        Primitive::Int
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WildcardKind {
    /// `?`
    Unbounded,
    /// `? extends T`
    Extends,
    /// `? super T`
    Super,
}

/// A type variable use. `bounds` is populated when the declaration is known;
/// the types inside it carry no bounds of their own, which bounds the structure
/// of a self-referential declaration such as `E extends Comparable<? super E>`.
#[derive(Debug, Clone, Eq)]
pub struct TypeVar {
    pub name: Name,
    /// Binary name of the declaring class, for display and diagnostics.
    pub owner: Option<Name>,
    pub bounds: Vec<Type>,
}

/// A type variable is identified by its declaration, not by whether the copy
/// in hand happens to carry its bounds: the `E` inside `Comparable<? super E>`
/// and the `E` of the enclosing class are the same variable, and comparing
/// bounds too would make them differ.
impl PartialEq for TypeVar {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.owner == other.owner
    }
}

impl std::hash::Hash for TypeVar {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.owner.hash(state);
    }
}

impl TypeVar {
    pub fn new(name: Name, owner: Option<Name>, bounds: Vec<Type>) -> TypeVar {
        TypeVar {
            name,
            owner,
            bounds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Primitive(Primitive),
    /// A class or interface type. `args` is empty for a non-generic class and
    /// for the raw use of a generic one; `Type::is_raw` tells them apart with
    /// the declaration in hand.
    Class {
        /// Binary name: `java.util.Map$Entry`, `ds.BinarySearchTree`.
        name: Name,
        args: Vec<Type>,
    },
    Array(Box<Type>),
    Var(Box<TypeVar>),
    Wildcard {
        kind: WildcardKind,
        bound: Option<Box<Type>>,
    },
    /// The type of the `null` literal.
    Null,
    /// Resolution did not succeed. Propagating this rather than guessing is
    /// what keeps the server from inventing diagnostics or completions.
    Unknown,
}

impl Type {
    pub const INT: Type = Type::Primitive(Primitive::Int);
    pub const BOOLEAN: Type = Type::Primitive(Primitive::Boolean);
    pub const VOID: Type = Type::Primitive(Primitive::Void);

    pub fn class(name: impl Into<Name>) -> Type {
        Type::Class {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn generic(name: impl Into<Name>, args: Vec<Type>) -> Type {
        Type::Class {
            name: name.into(),
            args,
        }
    }

    pub fn object() -> Type {
        Type::class("java.lang.Object")
    }

    pub fn string() -> Type {
        Type::class("java.lang.String")
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Type::Unknown)
    }

    pub fn is_void(&self) -> bool {
        matches!(self, Type::Primitive(Primitive::Void))
    }

    /// True when any part of the type failed to resolve. Callers use this to
    /// stay silent instead of reporting a diagnostic they cannot justify.
    pub fn has_unknown(&self) -> bool {
        match self {
            Type::Unknown => true,
            Type::Class { args, .. } => args.iter().any(Type::has_unknown),
            Type::Array(inner) => inner.has_unknown(),
            Type::Wildcard { bound, .. } => bound.as_deref().is_some_and(Type::has_unknown),
            Type::Var(var) => var.bounds.iter().any(Type::has_unknown),
            _ => false,
        }
    }

    pub fn class_name(&self) -> Option<&Name> {
        match self {
            Type::Class { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn type_args(&self) -> &[Type] {
        match self {
            Type::Class { args, .. } => args,
            _ => &[],
        }
    }

    /// Erasure (JLS 4.6): type arguments are dropped and a type variable
    /// becomes the erasure of its leftmost bound.
    pub fn erase(&self) -> Type {
        match self {
            Type::Class { name, .. } => Type::class(name.clone()),
            Type::Array(inner) => Type::Array(Box::new(inner.erase())),
            Type::Var(var) => var
                .bounds
                .first()
                .map(Type::erase)
                .unwrap_or_else(Type::object),
            Type::Wildcard { bound, kind } => match (kind, bound) {
                (WildcardKind::Extends, Some(bound)) => bound.erase(),
                _ => Type::object(),
            },
            other => other.clone(),
        }
    }

    /// Replaces type variables by name. Used to carry a receiver's type
    /// arguments into the members it inherits.
    pub fn substitute(&self, map: &Substitution) -> Type {
        if map.is_empty() {
            return self.clone();
        }
        match self {
            Type::Var(var) => match map.get(&var.name) {
                Some(replacement) => replacement.clone(),
                None => self.clone(),
            },
            Type::Class { name, args } => Type::Class {
                name: name.clone(),
                args: args.iter().map(|a| a.substitute(map)).collect(),
            },
            Type::Array(inner) => Type::Array(Box::new(inner.substitute(map))),
            Type::Wildcard { kind, bound } => Type::Wildcard {
                kind: kind.clone(),
                bound: bound.as_ref().map(|b| Box::new(b.substitute(map))),
            },
            other => other.clone(),
        }
    }

    /// The type a wildcard or variable stands in for when a concrete type is
    /// needed, for example to look up members on it.
    pub fn upper_bound(&self) -> Type {
        match self {
            Type::Wildcard {
                kind: WildcardKind::Extends,
                bound: Some(bound),
            } => bound.upper_bound(),
            Type::Wildcard { .. } => Type::object(),
            Type::Var(var) => var
                .bounds
                .first()
                .map(Type::upper_bound)
                .unwrap_or_else(Type::object),
            other => other.clone(),
        }
    }

    /// Human-readable form, using simple class names: `List<String>`.
    pub fn display_short(&self) -> String {
        self.render(true)
    }

    /// Human-readable form with qualified class names.
    pub fn display_full(&self) -> String {
        self.render(false)
    }

    fn render(&self, short: bool) -> String {
        match self {
            Type::Primitive(p) => p.as_str().to_string(),
            Type::Class { name, args } => {
                let base = if short {
                    crate::name::simple_name(name).to_string()
                } else {
                    crate::name::source_name(name)
                };
                if args.is_empty() {
                    base
                } else {
                    let rendered: Vec<String> = args.iter().map(|a| a.render(short)).collect();
                    format!("{base}<{}>", rendered.join(", "))
                }
            }
            Type::Array(inner) => format!("{}[]", inner.render(short)),
            Type::Var(var) => var.name.to_string(),
            Type::Wildcard { kind, bound } => match (kind, bound) {
                (WildcardKind::Extends, Some(b)) => format!("? extends {}", b.render(short)),
                (WildcardKind::Super, Some(b)) => format!("? super {}", b.render(short)),
                _ => "?".to_string(),
            },
            Type::Null => "null".to_string(),
            Type::Unknown => "<unknown>".to_string(),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_short())
    }
}

/// A mapping from type-variable names to types, as a short association list:
/// a class rarely declares more than a handful of parameters, and a linear
/// scan over two or three entries beats hashing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Substitution {
    entries: Vec<(Name, Type)>,
}

impl Substitution {
    pub fn new() -> Substitution {
        Substitution::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn insert(&mut self, name: Name, ty: Type) {
        match self.entries.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = ty,
            None => self.entries.push((name, ty)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Type> {
        self.entries
            .iter()
            .find(|(n, _)| &**n == name)
            .map(|(_, t)| t)
    }

    /// This substitution without the given names: what remains when a method's
    /// own type parameters shadow the class's parameters of the same name.
    pub fn without(&self, names: &[crate::symbols::TypeParam]) -> Substitution {
        Substitution {
            entries: self
                .entries
                .iter()
                .filter(|(name, _)| !names.iter().any(|param| param.name == *name))
                .cloned()
                .collect(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Type)> {
        self.entries.iter().map(|(n, t)| (n, t))
    }

    /// Composes with another substitution: the result applies `self`, then
    /// `outer`, which is what walking one level up a hierarchy requires.
    pub fn then(&self, outer: &Substitution) -> Substitution {
        let mut entries: Vec<(Name, Type)> = self
            .entries
            .iter()
            .map(|(n, t)| (n.clone(), t.substitute(outer)))
            .collect();
        for (name, ty) in &outer.entries {
            if !entries.iter().any(|(n, _)| n == name) {
                entries.push((name.clone(), ty.clone()));
            }
        }
        Substitution { entries }
    }
}

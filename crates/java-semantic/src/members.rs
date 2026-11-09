//! Member lookup across a type hierarchy, with type arguments carried through.
//!
//! This is the step that makes generics useful: looking up `add` on
//! `ArrayList<String>` walks to `List<E>` and returns `add(String)`, not
//! `add(E)` and not `add(Object)`. Each step up the hierarchy composes the
//! substitution so far with the one the supertype reference introduces.

use crate::db::Db;
use crate::name::Name;
use crate::symbols::{ClassKind, ClassRef, FieldData, MethodData, Visibility};
use crate::types::{Substitution, Type};
use crate::MAX_HIERARCHY_DEPTH;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// A member found on a type, with its signature already substituted.
#[derive(Debug, Clone)]
pub struct FoundField {
    pub owner: ClassRef,
    pub field: FieldData,
    /// How far up the hierarchy it was found; 0 means declared on the receiver.
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct FoundMethod {
    pub owner: ClassRef,
    pub method: MethodData,
    pub depth: u32,
    /// Position of the declaration in `owner.methods`.
    ///
    /// This is the method's identity. A signature cannot serve: `method` has
    /// had the receiver's type arguments substituted into it, so two
    /// same-arity overloads can end up looking alike.
    pub index: u32,
}

impl FoundMethod {
    /// The method as declared, before substitution, for display when the
    /// declaration itself is what matters.
    pub fn is_inherited(&self) -> bool {
        self.depth > 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct Members {
    pub fields: Vec<FoundField>,
    pub methods: Vec<FoundMethod>,
    /// Binary names of nested classes reachable through the hierarchy.
    pub nested: Vec<Name>,
}

impl Members {
    pub fn field(&self, name: &str) -> Option<&FoundField> {
        self.fields.iter().find(|f| &*f.field.name == name)
    }

    /// Methods with a given name; see `ClassData::methods_named` for why this
    /// collects rather than returning an iterator.
    pub fn methods_named(&self, name: &str) -> Vec<&FoundMethod> {
        self.methods
            .iter()
            .filter(|m| &*m.method.name == name)
            .collect()
    }
}

/// Collects the members visible on a type.
///
/// `include_static` and `include_instance` select which half of the members to
/// return, which is how `Foo.` offers statics while `foo.` offers instance
/// members.
pub fn members_of(db: &Db, ty: &Type, options: LookupOptions) -> Members {
    members_of_declared(db, ty, options, None)
}

/// `members_of` for a receiver whose class declaration is already in hand (a file's own class).
pub fn members_of_declared(
    db: &Db,
    ty: &Type,
    options: LookupOptions,
    root: Option<&ClassRef>,
) -> Members {
    let mut out = Members::default();
    let mut seen_classes = FxHashSet::default();
    let mut seen_fields = FxHashSet::default();
    let mut seen_methods: FxHashMap<String, usize> = FxHashMap::default();
    collect(
        db,
        ty,
        &Substitution::new(),
        0,
        &options,
        &mut seen_classes,
        &mut seen_fields,
        &mut seen_methods,
        &mut out,
        root,
    );
    out
}

/// True when `new` is a strict subtype of `old`, both fully known classes.
fn covariant_return(db: &Db, new: &Type, old: &Type) -> bool {
    matches!((new, old), (Type::Class { .. }, Type::Class { .. }))
        && new != old
        && !new.has_unknown()
        && !old.has_unknown()
        && is_subtype(db, new, old)
}

#[derive(Debug, Clone, Copy)]
pub struct LookupOptions {
    pub include_static: bool,
    pub include_instance: bool,
    /// Package of the requesting code, for package-private visibility.
    pub from_package: Option<&'static str>,
    /// Whether private members of the receiver's own class are visible.
    pub include_private: bool,
    /// Whether synthetic members (bridges, captured outer references) appear.
    pub include_synthetic: bool,
}

impl Default for LookupOptions {
    fn default() -> Self {
        LookupOptions {
            include_static: true,
            include_instance: true,
            from_package: None,
            include_private: false,
            include_synthetic: false,
        }
    }
}

impl LookupOptions {
    pub fn instance() -> LookupOptions {
        LookupOptions {
            include_static: false,
            ..LookupOptions::default()
        }
    }

    pub fn statics() -> LookupOptions {
        LookupOptions {
            include_instance: false,
            ..LookupOptions::default()
        }
    }

    /// Lookup from inside the class itself, where private members are visible.
    pub fn internal() -> LookupOptions {
        LookupOptions {
            include_private: true,
            ..LookupOptions::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(
    db: &Db,
    ty: &Type,
    subst: &Substitution,
    depth: u32,
    options: &LookupOptions,
    seen_classes: &mut FxHashSet<Name>,
    seen_fields: &mut FxHashSet<Name>,
    seen_methods: &mut FxHashMap<String, usize>,
    out: &mut Members,
    root: Option<&ClassRef>,
) {
    if depth > MAX_HIERARCHY_DEPTH {
        return;
    }
    match ty {
        Type::Array(_) => {
            // Arrays expose `length`, `clone()`, and Object's methods.
            if options.include_instance {
                out.fields.push(FoundField {
                    owner: array_pseudo_class(),
                    field: array_length_field(),
                    depth,
                });
            }
            if let Some(object) = db.class("java.lang.Object") {
                let object_type = Type::class(object.name.clone());
                collect(
                    db,
                    &object_type,
                    subst,
                    depth + 1,
                    options,
                    seen_classes,
                    seen_fields,
                    seen_methods,
                    out,
                    None,
                );
            }
        }
        Type::Primitive(_) | Type::Null | Type::Unknown => {}
        Type::Var(var) => {
            // A type variable's members are those of its bounds; with no
            // bound, Object's.
            if var.bounds.is_empty() {
                let object = Type::object();
                collect(
                    db,
                    &object,
                    subst,
                    depth,
                    options,
                    seen_classes,
                    seen_fields,
                    seen_methods,
                    out,
                    None,
                );
            }
            for bound in &var.bounds {
                collect(
                    db,
                    bound,
                    subst,
                    depth,
                    options,
                    seen_classes,
                    seen_fields,
                    seen_methods,
                    out,
                    None,
                );
            }
        }
        Type::Wildcard { .. } => {
            let bound = ty.upper_bound();
            collect(
                db,
                &bound,
                subst,
                depth,
                options,
                seen_classes,
                seen_fields,
                seen_methods,
                out,
                None,
            );
        }
        Type::Class { name, args } => {
            if !seen_classes.insert(name.clone()) {
                return;
            }
            // The receiver's own declaration, when the caller knows it, beats the project-wide index,
            // which keeps only one of several modules' classes of the same name.
            let class = match root {
                Some(declared) if declared.name == *name && depth == 0 => declared.clone(),
                _ => match db.class(name) {
                    Some(found) => found,
                    None => return,
                },
            };
            // The receiver's type arguments bind this class's parameters; the
            // substitution collected so far applies to those arguments.
            let own: Vec<Type> = args.iter().map(|a| a.substitute(subst)).collect();
            let local = class.substitution_for(&own);

            for field in &class.fields {
                if !visible(
                    field.visibility,
                    field.modifiers.is_synthetic,
                    depth,
                    options,
                ) || !wanted(field.modifiers.is_static, options)
                {
                    continue;
                }
                // A field in a subclass hides one of the same name above it.
                if !seen_fields.insert(field.name.clone()) {
                    continue;
                }
                let mut substituted = field.clone();
                substituted.ty = field.ty.substitute(&local);
                out.fields.push(FoundField {
                    owner: class.clone(),
                    field: substituted,
                    depth,
                });
            }

            for (position, method) in class.methods.iter().enumerate() {
                if method.is_constructor {
                    continue;
                }
                if !visible(
                    method.visibility,
                    method.modifiers.is_synthetic,
                    depth,
                    options,
                ) || !wanted(method.modifiers.is_static, options)
                {
                    continue;
                }
                // A static method cannot see its class's type parameters, and an
                // instance method's own `<T>` shadows a class `T`: either way
                // the receiver's arguments must not reach into the signature.
                let applicable = if method.modifiers.is_static {
                    Substitution::new()
                } else {
                    local.without(&method.type_params)
                };
                let mut substituted = method.clone();
                for param in &mut substituted.params {
                    param.ty = param.ty.substitute(&applicable);
                }
                substituted.ret = method.ret.substitute(&applicable);
                for thrown in &mut substituted.throws {
                    *thrown = thrown.substitute(&applicable);
                }
                // An override replaces the inherited declaration. Erasure is
                // the right key: that is what the JLS compares.
                let signature = substituted.erased_signature();
                if let Some(&existing) = seen_methods.get(&signature) {
                    // The same method reached through another supertype may declare a more
                    // specific (covariant) return type, and that is the declaration a caller
                    // sees: `MutableHttpResponse.getHeaders()` yields `MutableHttpHeaders`
                    // even though `HttpResponse.getHeaders()` is found first.
                    let found = &mut out.methods[existing];
                    if found.owner.name != class.name
                        && covariant_return(db, &substituted.ret, &found.method.ret)
                    {
                        found.owner = class.clone();
                        found.method = substituted;
                        found.index = position as u32;
                    }
                    continue;
                }
                seen_methods.insert(signature, out.methods.len());
                out.methods.push(FoundMethod {
                    owner: class.clone(),
                    method: substituted,
                    depth,
                    index: position as u32,
                });
            }

            for nested in &class.nested {
                if !out.nested.contains(nested) {
                    out.nested.push(nested.clone());
                }
            }

            let next = local.then(subst);
            let mut supertypes: Vec<&Type> = Vec::new();
            supertypes.extend(class.superclass.iter());
            supertypes.extend(class.interfaces.iter());
            // An interface implicitly inherits Object's public methods.
            let object = Type::object();
            if class.kind == ClassKind::Interface && depth == 0 {
                supertypes.push(&object);
            }
            for supertype in supertypes {
                let resolved = supertype.substitute(&next);
                collect(
                    db,
                    &resolved,
                    &Substitution::new(),
                    depth + 1,
                    options,
                    seen_classes,
                    seen_fields,
                    seen_methods,
                    out,
                    None,
                );
            }
        }
    }
}

fn wanted(is_static: bool, options: &LookupOptions) -> bool {
    if is_static {
        options.include_static
    } else {
        options.include_instance
    }
}

fn visible(
    visibility: Visibility,
    is_synthetic: bool,
    depth: u32,
    options: &LookupOptions,
) -> bool {
    if is_synthetic && !options.include_synthetic {
        return false;
    }
    match visibility {
        Visibility::Public | Visibility::Protected => true,
        // A private member is reachable only on the class that declares it.
        Visibility::Private => options.include_private && depth == 0,
        Visibility::Package => true,
    }
}

/// `length` is not a real field; it is synthesised so hover and completion can
/// describe it like one.
fn array_length_field() -> FieldData {
    FieldData {
        name: crate::name::name("length"),
        ty: Type::INT,
        visibility: Visibility::Public,
        modifiers: crate::symbols::Modifiers {
            is_final: true,
            ..Default::default()
        },
        origin: crate::symbols::Origin::Synthetic,
        doc: Some(Arc::from("The length of the array.")),
        constant_value: None,
    }
}

fn array_pseudo_class() -> ClassRef {
    Arc::new(crate::symbols::ClassData {
        name: crate::name::name("[]"),
        kind: ClassKind::Class,
        visibility: Visibility::Public,
        modifiers: Default::default(),
        type_params: Vec::new(),
        superclass: Some(Type::object()),
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        nested: Vec::new(),
        origin: crate::symbols::Origin::Synthetic,
        doc: None,
    })
}

/// The single abstract method of a functional interface, with the receiver's
/// type arguments applied.
///
/// A functional interface has exactly one abstract method that is not already
/// declared by `Object` (JLS 9.8). This is what gives a lambda's parameters
/// their types.
pub fn functional_interface_method(db: &Db, ty: &Type) -> Option<FoundMethod> {
    let class = db.class(ty.class_name()?)?;
    if !class.is_interface() {
        return None;
    }
    let members = members_of(
        db,
        ty,
        LookupOptions {
            include_static: false,
            include_instance: true,
            ..LookupOptions::default()
        },
    );
    let mut abstract_methods = members.methods.into_iter().filter(|found| {
        found.method.modifiers.is_abstract
            && !found.method.modifiers.is_default
            && !found.method.modifiers.is_static
            // A redeclaration of an Object method does not count.
            && !is_object_method(&found.method)
    });
    let only = abstract_methods.next()?;
    abstract_methods.next().is_none().then_some(only)
}

fn is_object_method(method: &MethodData) -> bool {
    match (&*method.name, method.params.len()) {
        ("equals", 1) => method.params[0]
            .ty
            .class_name()
            .is_some_and(|name| &**name == "java.lang.Object"),
        ("hashCode", 0) | ("toString", 0) => true,
        _ => false,
    }
}

/// Walks the supertypes of a class, breadth-first, yielding each as a type with
/// substitutions applied. Used by diagnostics and by `isSubtypeOf`.
pub fn supertypes(db: &Db, ty: &Type) -> Vec<Type> {
    let mut out = Vec::new();
    let mut queue = vec![(ty.clone(), 0u32)];
    let mut seen = FxHashSet::default();
    while let Some((current, depth)) = queue.pop() {
        if depth > MAX_HIERARCHY_DEPTH {
            continue;
        }
        let Type::Class { name, args } = &current else {
            let bound = current.upper_bound();
            if !matches!(bound, Type::Class { .. }) {
                continue;
            }
            queue.push((bound, depth));
            continue;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        if depth > 0 {
            out.push(current.clone());
        }
        let Some(class) = db.class(name) else {
            continue;
        };
        let subst = class.substitution_for(args);
        for supertype in class.superclass.iter().chain(&class.interfaces) {
            queue.push((supertype.substitute(&subst), depth + 1));
        }
    }
    out
}

/// Whether `from` is assignable to `to` by reference widening, ignoring
/// generics (that is, on erasures). Conservative: an unknown type is assumed
/// compatible so that unresolved code produces no diagnostics.
pub fn is_subtype(db: &Db, from: &Type, to: &Type) -> bool {
    if from.has_unknown() || to.has_unknown() {
        return true;
    }
    let from = from.erase();
    let to = to.erase();
    if from == to {
        return true;
    }
    match (&from, &to) {
        (Type::Null, Type::Class { .. } | Type::Array(_)) => true,
        (Type::Array(a), Type::Array(b)) => is_subtype(db, a, b),
        // Every array is an Object and is Cloneable and Serializable.
        (Type::Array(_), Type::Class { name, .. }) => matches!(
            &**name,
            "java.lang.Object" | "java.lang.Cloneable" | "java.io.Serializable"
        ),
        (Type::Class { .. }, Type::Class { name: to_name, .. }) => {
            if &**to_name == "java.lang.Object" {
                return true;
            }
            supertypes(db, &from)
                .iter()
                .any(|s| s.class_name().map(|n| n == to_name).unwrap_or(false))
        }
        _ => false,
    }
}

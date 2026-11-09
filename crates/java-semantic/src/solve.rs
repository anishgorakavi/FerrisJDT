//! Solving a generic method's type variables from what a call supplies.
//!
//! This is the pragmatic subset of JLS 18 that covers coursework and the
//! standard library's collection and stream APIs: bind each type variable from
//! the arguments (including varargs and wildcard-parameterised ones), then from
//! the type the call is expected to produce, then from the bodies of lambdas
//! and method references. A variable nothing determines is reported as unknown
//! rather than guessed, so an unsolved call never produces a false diagnostic.

use crate::db::Db;
use crate::members::{is_subtype, supertypes};
use crate::name::Name;
use crate::symbols::{MethodData, TypeParam};
use crate::types::{Primitive, Substitution, Type, WildcardKind};

/// How deep a declared type is searched for variables. Types this nested are
/// not written by hand, and the bound keeps a pathological one cheap.
const MAX_BIND_DEPTH: u32 = 16;

/// The bindings found so far for one call's type variables.
pub struct Inference<'d> {
    db: &'d Db,
    params: Vec<TypeParam>,
    solved: Substitution,
    /// Variables bound to differing types, which were joined to a common
    /// supertype; their declared bounds are not re-checked against the join.
    joined: Vec<Name>,
    /// When set, a binding is recorded only for a variable that has none.
    fill_only: bool,
}

impl<'d> Inference<'d> {
    pub fn new(db: &'d Db, params: &[TypeParam]) -> Inference<'d> {
        Inference {
            db,
            params: params.to_vec(),
            solved: Substitution::new(),
            joined: Vec::new(),
            fill_only: false,
        }
    }

    /// Whether `name` is one of the variables being solved.
    fn is_ours(&self, name: &str) -> bool {
        self.params.iter().any(|param| &*param.name == name)
    }

    /// The bindings so far, for substituting into a type that is still being worked out.
    pub fn solved(&self) -> &Substitution {
        &self.solved
    }

    /// The bindings so far, with every unsolved variable mapped to `Unknown`.
    /// A type rewritten this way can be used as an expected type for a nested
    /// call without leaking this call's free variables into it.
    pub fn open_substitution(&self) -> Substitution {
        let mut open = self.solved.clone();
        for param in &self.params {
            if !self.is_solved(&param.name) {
                open.insert(param.name.clone(), Type::Unknown);
            }
        }
        open
    }

    /// Whether a variable has a fully known binding.
    pub fn is_solved(&self, name: &str) -> bool {
        self.solved.get(name).is_some_and(|ty| !ty.has_unknown())
    }

    /// Whether `ty` still mentions a variable with no fully known binding.
    pub fn mentions_unsolved(&self, ty: &Type) -> bool {
        match ty {
            Type::Var(var) => self.is_ours(&var.name) && !self.is_solved(&var.name),
            Type::Class { args, .. } => args.iter().any(|a| self.mentions_unsolved(a)),
            Type::Array(inner) => self.mentions_unsolved(inner),
            Type::Wildcard {
                bound: Some(bound), ..
            } => self.mentions_unsolved(bound),
            _ => false,
        }
    }

    /// Binds variables in `declared` so that it accepts `actual`.
    pub fn bind(&mut self, declared: &Type, actual: &Type) {
        self.bind_at(declared, actual, 0);
    }

    /// Binds the variables of a method's return type from the type the call is
    /// expected to have. Variables the arguments already fixed are left alone.
    pub fn bind_expected(&mut self, declared_return: &Type, expected: &Type) {
        if expected.is_unknown() {
            return;
        }
        let was = std::mem::replace(&mut self.fill_only, true);
        match (declared_return, expected) {
            // The expected type only bounds the result from above, so it cannot fix a
            // variable whose declared bound it does not satisfy (`Object o = max(...)`
            // with `E extends Number` leaves `E` open, not `Object`).
            // A wildcard target is a capture variable javac would pick as the result; guessing a
            // concrete type for it would then fail the `? extends T` write check.
            (Type::Var(_), Type::Wildcard { .. }) => {}
            (Type::Var(var), _) => {
                if self.expected_fits_bounds(&var.name, expected) {
                    self.bind_at(declared_return, expected, 0);
                }
            }
            (_, Type::Class { name, args }) if !args.is_empty() => {
                if let Some(Type::Class { args: viewed, .. }) =
                    view_as(self.db, declared_return, name)
                {
                    for (declared, wanted) in viewed.iter().zip(args) {
                        self.bind_at(declared, wanted, 1);
                    }
                }
            }
            _ => {}
        }
        self.fill_only = was;
    }

    /// Whether `ty` is a subtype of every declared bound of the variable `name`, judged on erasures.
    fn expected_fits_bounds(&self, name: &str, ty: &Type) -> bool {
        if ty.has_unknown() || !matches!(ty, Type::Class { .. } | Type::Array(_)) {
            return true;
        }
        let Some(param) = self.params.iter().find(|p| &*p.name == name) else {
            return true;
        };
        param
            .bounds
            .iter()
            .all(|bound| is_object(bound) || is_subtype(self.db, ty, &bound.erase()))
    }

    fn bind_at(&mut self, declared: &Type, actual: &Type, depth: u32) {
        if depth > MAX_BIND_DEPTH {
            return;
        }
        match declared {
            Type::Var(var) if self.is_ours(&var.name) => self.record(&var.name.clone(), actual),
            Type::Array(declared_element) => {
                if let Type::Array(actual_element) = actual {
                    // `T[]` never matches `int[]`: a primitive array is not an array of T.
                    if !matches!(**actual_element, Type::Primitive(_)) {
                        self.bind_at(declared_element, actual_element, depth + 1);
                    }
                }
            }
            Type::Wildcard {
                bound: Some(bound), ..
            } => self.bind_at(bound, actual, depth + 1),
            Type::Class { name, args } if !args.is_empty() => {
                let actual = match actual {
                    Type::Primitive(p) if *p != Primitive::Void => Type::class(p.boxed()),
                    Type::Wildcard {
                        bound: Some(bound), ..
                    } => (**bound).clone(),
                    other => other.clone(),
                };
                if let Some(Type::Class { args: viewed, .. }) = view_as(self.db, &actual, name) {
                    for (declared_arg, actual_arg) in args.iter().zip(&viewed) {
                        if matches!(
                            actual_arg,
                            Type::Wildcard {
                                kind: WildcardKind::Unbounded,
                                ..
                            }
                        ) {
                            continue;
                        }
                        self.bind_at(declared_arg, actual_arg, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }

    /// Records one binding, joining it with an earlier one when they differ.
    fn record(&mut self, name: &Name, actual: &Type) {
        let candidate = match actual {
            Type::Unknown | Type::Null => return,
            Type::Primitive(Primitive::Void) => return,
            Type::Primitive(p) => Type::class(p.boxed()),
            Type::Wildcard {
                bound: Some(bound), ..
            } => (**bound).clone(),
            Type::Wildcard { .. } => return,
            other => other.clone(),
        };
        let existing = self.solved.get(name).cloned();
        match existing {
            None => self.solved.insert(name.clone(), candidate),
            Some(_) if self.fill_only && self.is_solved(name) => {}
            Some(existing) if existing == candidate => {}
            Some(existing) if existing.has_unknown() && !candidate.has_unknown() => {
                self.solved.insert(name.clone(), candidate);
            }
            Some(_) if candidate.has_unknown() => {}
            Some(existing) => {
                let joined = lub(self.db, &existing, &candidate);
                self.joined.push(name.clone());
                self.solved.insert(name.clone(), joined);
            }
        }
    }

    /// Whether the solved types satisfy their declared bounds, judged on
    /// erasures. A call that fails this is not applicable.
    pub fn bounds_hold(&self) -> bool {
        for param in &self.params {
            let Some(solved) = self.solved.get(&param.name) else {
                continue;
            };
            if self.joined.contains(&param.name)
                || solved.has_unknown()
                || !matches!(solved, Type::Class { .. } | Type::Array(_))
            {
                continue;
            }
            for bound in &param.bounds {
                let erased = bound.erase();
                let is_object = erased
                    .class_name()
                    .is_some_and(|n| &**n == "java.lang.Object");
                if !is_object && !is_subtype(self.db, solved, &erased) {
                    return false;
                }
            }
        }
        true
    }

    /// Completes the bindings. A variable nothing determined takes its declared
    /// bound when it has a useful one, and is otherwise unknown.
    pub fn finish(self) -> Substitution {
        let mut result = self.solved.clone();
        for param in &self.params {
            // A binding with unknown parts (`EvictingQueue<Unknown>`) is still closer to the truth
            // than the declared bound (`Collection<T>`), and diagnostics stand down on its unknowns.
            if self.solved.get(&param.name).is_some() {
                continue;
            }
            let fallback = match param.bounds.first() {
                Some(bound) if !is_object(bound) => {
                    // A bound that names the variable itself (`T extends Comparable<T>`)
                    // would leak the variable; its erasure is the honest answer.
                    if mentions_any(bound, &self.params) {
                        bound.erase()
                    } else {
                        bound.clone()
                    }
                }
                _ => Type::Unknown,
            };
            result.insert(param.name.clone(), fallback);
        }
        result
    }
}

fn is_object(ty: &Type) -> bool {
    ty.class_name().is_some_and(|n| &**n == "java.lang.Object")
}

fn mentions_any(ty: &Type, params: &[TypeParam]) -> bool {
    match ty {
        Type::Var(var) => params.iter().any(|p| p.name == var.name),
        Type::Class { args, .. } => args.iter().any(|a| mentions_any(a, params)),
        Type::Array(inner) => mentions_any(inner, params),
        Type::Wildcard {
            bound: Some(bound), ..
        } => mentions_any(bound, params),
        _ => false,
    }
}

/// Binds a method's parameters against call arguments, expanding a varargs
/// tail into one binding per extra argument.
pub fn bind_arguments(
    inference: &mut Inference<'_>,
    method: &MethodData,
    args: &[Type],
    spread: bool,
) {
    let params = &method.params;
    if !spread {
        for (param, arg) in params.iter().zip(args) {
            inference.bind(&param.ty, arg);
        }
        return;
    }
    let fixed = params.len() - 1;
    for (param, arg) in params[..fixed].iter().zip(args) {
        inference.bind(&param.ty, arg);
    }
    if let Type::Array(element) = &params[fixed].ty {
        for arg in args.iter().skip(fixed) {
            inference.bind(element, arg);
        }
    }
}

/// Whether a call passes its varargs as individual elements rather than as an array.
pub fn spreads_varargs(method: &MethodData, args: &[Type]) -> bool {
    if !method.modifiers.is_varargs || method.params.is_empty() {
        return false;
    }
    if args.len() != method.params.len() {
        return true;
    }
    let declared_element_is_primitive = matches!(
        &method.params[method.params.len() - 1].ty,
        Type::Array(element) if matches!(**element, Type::Primitive(_))
    );
    match &args[args.len() - 1] {
        Type::Null => false,
        Type::Array(element) => {
            matches!(**element, Type::Primitive(_)) && !declared_element_is_primitive
        }
        _ => true,
    }
}

/// `ty` seen as an instance of the class `target`, with type arguments carried
/// through: `ArrayList<String>` viewed as `Collection` is `Collection<String>`.
pub fn view_as(db: &Db, ty: &Type, target: &str) -> Option<Type> {
    match ty {
        Type::Class { name, .. } if &**name == target => Some(ty.clone()),
        Type::Class { .. } => supertypes(db, ty)
            .into_iter()
            .find(|s| s.class_name().is_some_and(|n| &**n == target)),
        Type::Var(var) => var
            .bounds
            .iter()
            .find_map(|bound| view_as(db, bound, target)),
        Type::Wildcard {
            kind: WildcardKind::Extends,
            bound: Some(bound),
        } => view_as(db, bound, target),
        _ => None,
    }
}

/// The closest common supertype of two types, preferring a class to an interface.
pub fn lub(db: &Db, left: &Type, right: &Type) -> Type {
    if is_subtype(db, left, right) {
        return right.clone();
    }
    if is_subtype(db, right, left) {
        return left.clone();
    }
    let mut candidates = supertypes(db, left);
    candidates.sort_by_key(|candidate| {
        let is_interface = candidate
            .class_name()
            .and_then(|name| db.class(name))
            .is_some_and(|class| class.is_interface());
        is_interface
    });
    candidates
        .into_iter()
        .find(|candidate| !is_object(candidate) && is_subtype(db, right, candidate))
        .unwrap_or_else(Type::object)
}

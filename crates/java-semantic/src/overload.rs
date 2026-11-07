//! Assignability and overload resolution.
//!
//! Method applicability follows the three phases of JLS 15.12.2: strict
//! invocation, then loose invocation with boxing, then variable arity. Each
//! phase is tried in turn and the most specific applicable candidate wins,
//! which is what makes `println(int)` and `println(String)` resolve correctly.
//!
//! Where a judgement cannot be made — an argument whose type did not resolve —
//! the answer is "compatible". Being permissive here means an incomplete file
//! produces no completion, never a wrong diagnostic.

use crate::db::Db;
use crate::members::{is_subtype, FoundMethod};
use crate::types::{Primitive, Substitution, Type};

/// Whether a value of type `from` may be assigned to `to` (JLS 5.2), including
/// boxing, unboxing, and widening.
pub fn is_assignable(db: &Db, from: &Type, to: &Type) -> bool {
    if from.has_unknown() || to.has_unknown() {
        return true;
    }
    if from == to {
        return true;
    }
    match (from, to) {
        (Type::Primitive(a), Type::Primitive(b)) => a.widens_to(*b),
        // Boxing, then reference widening: int -> Integer -> Number -> Object.
        (Type::Primitive(a), Type::Class { .. }) => {
            if *a == Primitive::Void {
                return false;
            }
            is_subtype(db, &Type::class(a.boxed()), to)
        }
        // Unboxing, then widening: Integer -> int -> long.
        (Type::Class { name, .. }, Type::Primitive(b)) => match Primitive::unboxed(name) {
            Some(unboxed) => unboxed.widens_to(*b),
            None => false,
        },
        (Type::Null, Type::Class { .. } | Type::Array(_)) => true,
        // A `? super T` target accepts anything assignable to T: the actual
        // type argument is some supertype of T, so T itself always fits. This
        // is the capture conversion of JLS 5.1.10 in the only shape that
        // matters in practice, and it is what makes `compareTo` usable on a
        // variable bounded by `Comparable<? super E>`.
        (
            _,
            Type::Wildcard {
                kind: crate::types::WildcardKind::Super,
                bound: Some(bound),
            },
        ) => is_assignable(db, from, bound),
        // An `? extends T` or bare `?` target cannot be written to safely.
        (_, Type::Wildcard { .. }) => matches!(from, Type::Null),
        (_, Type::Var(_)) => {
            // Assigning to a type variable is only safe for null or the
            // variable itself, which the equality check above covered.
            matches!(from, Type::Null)
        }
        (Type::Var(_) | Type::Wildcard { .. }, _) => is_assignable(db, &from.upper_bound(), to),
        _ => is_subtype(db, from, to),
    }
}

/// Strict invocation: subtyping and primitive widening only, no boxing.
fn is_assignable_strict(db: &Db, from: &Type, to: &Type) -> bool {
    if from.has_unknown() || to.has_unknown() {
        return true;
    }
    if from == to {
        return true;
    }
    match (from, to) {
        (Type::Primitive(a), Type::Primitive(b)) => a.widens_to(*b),
        (Type::Primitive(_), _) | (_, Type::Primitive(_)) => false,
        _ => is_assignable(db, from, to),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Strict,
    Loose,
    Varargs,
}

fn applicable(db: &Db, method: &FoundMethod, args: &[Type], phase: Phase) -> bool {
    let params = &method.method.params;
    match phase {
        Phase::Strict | Phase::Loose => {
            if params.len() != args.len() {
                return false;
            }
            args.iter().zip(params).all(|(arg, param)| match phase {
                Phase::Strict => is_assignable_strict(db, arg, &param.ty),
                _ => is_assignable(db, arg, &param.ty),
            })
        }
        Phase::Varargs => {
            if !method.method.modifiers.is_varargs || params.is_empty() {
                return false;
            }
            let fixed = params.len() - 1;
            if args.len() < fixed {
                return false;
            }
            if !args[..fixed]
                .iter()
                .zip(&params[..fixed])
                .all(|(arg, param)| is_assignable(db, arg, &param.ty))
            {
                return false;
            }
            let Type::Array(element) = &params[fixed].ty else {
                return false;
            };
            // The variable-arity part accepts either an array of the element
            // type or any number of individual elements.
            if args.len() == params.len() && is_assignable(db, &args[fixed], &params[fixed].ty) {
                return true;
            }
            args[fixed..]
                .iter()
                .all(|arg| is_assignable(db, arg, element))
        }
    }
}

/// Picks the method an invocation refers to.
///
/// Returns `None` when nothing is applicable, and picks the most specific
/// candidate when several are. Ties are broken by preferring the one declared
/// closest to the receiver, which keeps an override ahead of what it overrides.
pub fn select<'m>(
    db: &Db,
    candidates: &'m [&'m FoundMethod],
    args: &[Type],
) -> Option<&'m FoundMethod> {
    for phase in [Phase::Strict, Phase::Loose, Phase::Varargs] {
        let round: Vec<&FoundMethod> = candidates
            .iter()
            .copied()
            .filter(|method| applicable(db, method, args, phase))
            .collect();
        if round.is_empty() {
            continue;
        }
        if round.len() == 1 {
            return Some(round[0]);
        }
        return Some(most_specific(db, &round));
    }
    None
}

/// Among applicable methods, the one whose parameters are assignable to every
/// other candidate's (JLS 15.12.2.5), falling back to the shallowest.
fn most_specific<'m>(db: &Db, round: &[&'m FoundMethod]) -> &'m FoundMethod {
    let mut best = round[0];
    for candidate in &round[1..] {
        if more_specific(db, candidate, best) {
            best = candidate;
        }
    }
    best
}

fn more_specific(db: &Db, left: &FoundMethod, right: &FoundMethod) -> bool {
    let (a, b) = (&left.method.params, &right.method.params);
    if a.len() != b.len() {
        // Fewer parameters means the fixed-arity form, which is preferred.
        return a.len() < b.len();
    }
    let left_into_right = a
        .iter()
        .zip(b)
        .all(|(x, y)| is_assignable(db, &x.ty, &y.ty));
    let right_into_left = b
        .iter()
        .zip(a)
        .all(|(x, y)| is_assignable(db, &x.ty, &y.ty));
    if left_into_right != right_into_left {
        return left_into_right;
    }
    left.depth < right.depth
}

/// Infers a generic method's type arguments from the argument types.
///
/// This is the pragmatic subset of JLS 18: match each parameter against the
/// corresponding argument and record what each type variable must be. It is
/// enough for the shapes that appear in ordinary code — `Arrays.asList("a")`,
/// `Collections.sort(list)`, `map.put(k, v)` — and any variable it cannot
/// solve is left alone rather than guessed.
pub fn infer_type_arguments(method: &crate::symbols::MethodData, args: &[Type]) -> Substitution {
    let mut subst = Substitution::new();
    if method.type_params.is_empty() {
        return subst;
    }
    let names: Vec<&str> = method.type_params.iter().map(|p| &*p.name).collect();
    for (param, arg) in method.params.iter().zip(args) {
        unify(&param.ty, arg, &names, &mut subst);
    }
    // Any variable still unsolved falls back to its own bound, which is what
    // erasure would give; this keeps the return type usable.
    for param in &method.type_params {
        if subst.get(&param.name).is_none() {
            let bound = param.bounds.first().cloned().unwrap_or_else(Type::object);
            subst.insert(param.name.clone(), bound);
        }
    }
    subst
}

/// Matches a declared type against an actual one, recording bindings for any
/// of `names` encountered.
fn unify(declared: &Type, actual: &Type, names: &[&str], subst: &mut Substitution) {
    match declared {
        Type::Var(var) if names.contains(&&*var.name) => {
            if subst.get(&var.name).is_none() && !actual.is_unknown() {
                // An argument's own type variables are kept; boxing a
                // primitive is what the call would do anyway.
                let bound = match actual {
                    Type::Primitive(p) if *p != Primitive::Void => Type::class(p.boxed()),
                    Type::Null => return,
                    other => other.clone(),
                };
                subst.insert(var.name.clone(), bound);
            }
        }
        Type::Array(declared_element) => {
            if let Type::Array(actual_element) = actual {
                unify(declared_element, actual_element, names, subst);
            }
        }
        Type::Class {
            args: declared_args,
            ..
        } => {
            // Match type arguments positionally against the actual type's,
            // looking through a wildcard to its bound.
            let actual = actual.upper_bound();
            let actual_args = actual.type_args();
            for (left, right) in declared_args.iter().zip(actual_args) {
                unify(&left.upper_bound(), &right.upper_bound(), names, subst);
            }
        }
        Type::Wildcard {
            bound: Some(bound), ..
        } => unify(bound, actual, names, subst),
        _ => {}
    }
}

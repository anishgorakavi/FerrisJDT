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
use crate::solve::{self, Inference};
use crate::types::{Primitive, Substitution, Type};

/// Whether a value of type `from` may be assigned to `to` (JLS 5.2), including
/// boxing, unboxing, and widening.
/// True when no resolution of an unknown part could make `from` fit `to`: a class or primitive
/// value is never an array. Without this check an unsolved `T[]` parameter, which is
/// `Unknown[]`, would accept anything and let a varargs overload outrank a fixed-arity one.
fn never_assignable(db: &Db, from: &Type, to: &Type) -> bool {
    if matches!(to, Type::Array(_)) && matches!(from, Type::Class { .. } | Type::Primitive(_)) {
        return true;
    }
    // Unknown type arguments say nothing about the classes themselves: a `List<String>` is never a
    // `Predicate<?>`, however `T` came out.
    matches!((from, to), (Type::Class { .. }, Type::Class { .. }))
        && !is_subtype(db, &from.erase(), &to.erase())
}

pub fn is_assignable(db: &Db, from: &Type, to: &Type) -> bool {
    if from.has_unknown() || to.has_unknown() {
        // Unknown parts say nothing about the classes themselves; only when no resolution could
        // make them fit is the answer no.
        return !never_assignable(db, from, to);
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
        // Reading from a `? extends B` is reading a B (capture conversion), so
        // `T best = items.get(0)` is fine for `List<? extends T>`.
        (
            Type::Wildcard {
                kind: crate::types::WildcardKind::Extends,
                bound: Some(bound),
            },
            _,
        ) if !matches!(to, Type::Wildcard { .. }) => is_assignable(db, bound, to),
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
        // Unknown parts say nothing about the classes themselves; only when no resolution could
        // make them fit is the answer no.
        return !never_assignable(db, from, to);
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

/// A method's parameter types as a call with these arguments would see them:
/// a generic method's own type variables are replaced by what the arguments
/// bind them to, so `T` in `id(T)` accepts a `String`. `None` when the solved
/// types break a declared bound.
pub fn instantiated_params(
    db: &Db,
    method: &FoundMethod,
    args: &[Type],
    spread: bool,
) -> Option<Vec<Type>> {
    let data = &method.method;
    if data.type_params.is_empty() {
        return Some(data.params.iter().map(|p| p.ty.clone()).collect());
    }
    let mut inference = Inference::new(db, &data.type_params);
    solve::bind_arguments(&mut inference, data, args, spread);
    if !inference.bounds_hold() {
        return None;
    }
    let solved = inference.finish();
    Some(
        data.params
            .iter()
            .map(|p| p.ty.substitute(&solved))
            .collect(),
    )
}

fn applicable(db: &Db, method: &FoundMethod, args: &[Type], phase: Phase) -> bool {
    let spread = phase == Phase::Varargs;
    let Some(params) = instantiated_params(db, method, args, spread) else {
        return false;
    };
    match phase {
        Phase::Strict | Phase::Loose => {
            if params.len() != args.len() {
                return false;
            }
            args.iter().zip(&params).all(|(arg, param)| match phase {
                Phase::Strict => is_assignable_strict(db, arg, param),
                _ => is_assignable(db, arg, param),
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
                .all(|(arg, param)| is_assignable(db, arg, param))
            {
                return false;
            }
            let Type::Array(element) = &params[fixed] else {
                return false;
            };
            // The variable-arity part accepts either an array of the element
            // type or any number of individual elements.
            if args.len() == params.len() && is_assignable(db, &args[fixed], &params[fixed]) {
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
    // The method being compared against is generic in its own variables, which stand for whatever
    // fits their bounds, so `List<String>` is more specific than a bare `T`.
    let bind = |method: &FoundMethod| {
        let mut bound = Substitution::new();
        for param in &method.method.type_params {
            let erased = param.bounds.first().map(Type::erase);
            bound.insert(param.name.clone(), erased.unwrap_or_else(Type::object));
        }
        bound
    };
    let (for_left, for_right) = (bind(left), bind(right));
    let left_into_right = a
        .iter()
        .zip(b)
        .all(|(x, y)| is_assignable(db, &x.ty, &y.ty.substitute(&for_right)));
    let right_into_left = b
        .iter()
        .zip(a)
        .all(|(x, y)| is_assignable(db, &x.ty, &y.ty.substitute(&for_left)));
    if left_into_right != right_into_left {
        return left_into_right;
    }
    left.depth < right.depth
}

/// Infers a generic method's type arguments from the argument types.
///
/// Covers the shapes that appear in ordinary code: `Arrays.asList("a")`,
/// `Collections.max(list)`, `map.put(k, v)`. Arguments that are lambdas or
/// calls whose own type depends on this one are handled by the caller, which
/// has the syntax; see `Inferer::apply_type_arguments`.
pub fn infer_type_arguments(
    db: &Db,
    method: &crate::symbols::MethodData,
    args: &[Type],
) -> Substitution {
    if method.type_params.is_empty() {
        return Substitution::new();
    }
    let mut inference = Inference::new(db, &method.type_params);
    let spread = solve::spreads_varargs(method, args);
    solve::bind_arguments(&mut inference, method, args, spread);
    inference.finish()
}

//! Constant narrowing (JLS 5.2): `byte b = 3;` and `return 1;` from a `byte` method.
//!
//! An `int` constant expression whose value fits in `byte`, `short` or `char` may be assigned to
//! that type (or its box) without a cast. Deciding that needs the expression's value, so this
//! module folds the integer operators over literals. Names might be constant variables, which the
//! checker cannot tell apart from other fields, so an expression that mentions one is given the
//! benefit of the doubt: a false error on valid code is worse than a missed one.

use java_semantic::types::Primitive;
use java_semantic::Type;
use java_syntax::{ast, Node};

/// What is known about an expression's constant value.
enum Const {
    /// A compile-time constant with this value.
    Known(i64),
    /// Might be a constant (it names a variable or has an operand we cannot fold).
    Maybe,
    /// Certainly not a constant (a call, an assignment, ...).
    Not,
}

/// True when assigning `value` (of type `actual`) to `declared` is legal only by constant narrowing.
pub fn narrows_by_constant(value: Node<'_>, source: &str, actual: &Type, declared: &Type) -> bool {
    let target = match declared {
        Type::Primitive(p) => *p,
        Type::Class { name, .. } => match Primitive::unboxed(name) {
            Some(p) => p,
            None => return false,
        },
        _ => return false,
    };
    let range = match target {
        Primitive::Byte => i64::from(i8::MIN)..=i64::from(i8::MAX),
        Primitive::Short => i64::from(i16::MIN)..=i64::from(i16::MAX),
        Primitive::Char => 0..=i64::from(u16::MAX),
        _ => return false,
    };
    if !matches!(
        actual,
        Type::Primitive(Primitive::Int | Primitive::Short | Primitive::Char | Primitive::Byte)
    ) {
        return false;
    }
    match fold(value, source) {
        Const::Known(v) => range.contains(&v),
        Const::Maybe => true,
        Const::Not => false,
    }
}

/// Folds `node` as far as possible.
fn fold(node: Node<'_>, source: &str) -> Const {
    let text = ast::text(node, source);
    match node.kind() {
        "decimal_integer_literal"
        | "hex_integer_literal"
        | "octal_integer_literal"
        | "binary_integer_literal" => integer_literal(text),
        "character_literal" => char_literal(text),
        "parenthesized_expression" => match ast::named_children(node).next() {
            Some(inner) => fold(inner, source),
            None => Const::Maybe,
        },
        "unary_expression" => {
            let Some(operand) = ast::field(node, "operand") else {
                return Const::Maybe;
            };
            let operator = ast::field(node, "operator").map_or("", |o| ast::text(o, source));
            match (fold(operand, source), operator) {
                (Const::Known(v), "-") => Const::Known(-v),
                (Const::Known(v), "+") => Const::Known(v),
                (Const::Known(v), "~") => Const::Known(!v),
                (Const::Not, _) => Const::Not,
                _ => Const::Maybe,
            }
        }
        "binary_expression" => {
            let (Some(left), Some(right)) = (ast::field(node, "left"), ast::field(node, "right"))
            else {
                return Const::Maybe;
            };
            let operator = ast::field(node, "operator").map_or("", |o| ast::text(o, source));
            match (fold(left, source), fold(right, source)) {
                (Const::Not, _) | (_, Const::Not) => Const::Not,
                (Const::Known(a), Const::Known(b)) => binary(a, operator, b),
                _ => Const::Maybe,
            }
        }
        "cast_expression" => {
            let Some(value) = ast::field(node, "value") else {
                return Const::Maybe;
            };
            match (fold(value, source), cast_target(node, source)) {
                (Const::Not, _) => Const::Not,
                (Const::Known(v), Some(Primitive::Byte)) => Const::Known(i64::from(v as i8)),
                (Const::Known(v), Some(Primitive::Short)) => Const::Known(i64::from(v as i16)),
                (Const::Known(v), Some(Primitive::Char)) => Const::Known(i64::from(v as u16)),
                (Const::Known(v), Some(Primitive::Int)) => Const::Known(i64::from(v as i32)),
                _ => Const::Maybe,
            }
        }
        "ternary_expression" => {
            let parts = ["condition", "consequence", "alternative"]
                .map(|field| ast::field(node, field).map(|n| fold(n, source)));
            if parts.iter().flatten().any(|c| matches!(c, Const::Not)) {
                Const::Not
            } else {
                Const::Maybe
            }
        }
        "identifier" | "field_access" | "scoped_identifier" | "true" | "false" => Const::Maybe,
        _ => Const::Not,
    }
}

/// The primitive a cast converts to, if it is one.
fn cast_target(node: Node<'_>, source: &str) -> Option<Primitive> {
    let ty = ast::field(node, "type")?;
    match ast::text(ty, source) {
        "byte" => Some(Primitive::Byte),
        "short" => Some(Primitive::Short),
        "char" => Some(Primitive::Char),
        "int" => Some(Primitive::Int),
        _ => None,
    }
}

/// Applies a binary operator with `int` semantics; anything unusual stays `Maybe`.
fn binary(a: i64, operator: &str, b: i64) -> Const {
    let (x, y) = (a as i32, b as i32);
    let value = match operator {
        "+" => x.wrapping_add(y),
        "-" => x.wrapping_sub(y),
        "*" => x.wrapping_mul(y),
        "/" if y != 0 => x.wrapping_div(y),
        "%" if y != 0 => x.wrapping_rem(y),
        "&" => x & y,
        "|" => x | y,
        "^" => x ^ y,
        "<<" => x.wrapping_shl(y as u32 & 31),
        ">>" => x.wrapping_shr(y as u32 & 31),
        ">>>" => ((x as u32).wrapping_shr(y as u32 & 31)) as i32,
        _ => return Const::Maybe,
    };
    Const::Known(i64::from(value))
}

/// Parses an integer literal; a `long` literal or an unparsable one is `Maybe`.
fn integer_literal(text: &str) -> Const {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    if digits.ends_with(['l', 'L']) {
        return Const::Maybe;
    }
    let lower = digits.to_ascii_lowercase();
    let parsed = if let Some(hex) = lower.strip_prefix("0x") {
        i64::from_str_radix(hex, 16)
    } else if let Some(bin) = lower.strip_prefix("0b") {
        i64::from_str_radix(bin, 2)
    } else if let Some(oct) = lower.strip_prefix("0o") {
        i64::from_str_radix(oct, 8)
    } else if lower.len() > 1 && lower.starts_with('0') {
        i64::from_str_radix(&lower[1..], 8)
    } else {
        lower.parse()
    };
    match parsed {
        // Hex, octal and binary int literals wrap into the negative range at 2^31.
        Ok(v) if v <= i64::from(u32::MAX) && !lower.starts_with(|c: char| c != '0') => {
            Const::Known(i64::from(v as u32 as i32))
        }
        Ok(v) if v <= i64::from(i32::MAX) => Const::Known(v),
        _ => Const::Maybe,
    }
}

/// Parses a character literal such as `'a'`, `'\n'` or `'A'`; odd forms are `Maybe`.
fn char_literal(text: &str) -> Const {
    let inner = text.trim_matches('\'');
    let mut chars = inner.chars();
    let value = match (chars.next(), chars.next()) {
        (Some('\\'), Some(escape)) => match escape {
            'n' => 10,
            't' => 9,
            'r' => 13,
            'b' => 8,
            'f' => 12,
            's' => 32,
            '0' if inner.len() == 2 => 0,
            '\\' | '\'' | '"' => escape as i64,
            'u' => match u32::from_str_radix(inner.trim_start_matches("\\u"), 16) {
                Ok(v) => i64::from(v),
                Err(_) => return Const::Maybe,
            },
            _ => return Const::Maybe,
        },
        (Some(c), None) => c as i64,
        _ => return Const::Maybe,
    };
    Const::Known(value)
}

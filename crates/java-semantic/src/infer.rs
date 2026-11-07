//! Expression type inference.
//!
//! Types are computed on demand from the syntax tree; nothing is stored on
//! nodes. An expression whose type cannot be determined yields `Type::Unknown`,
//! and that value is propagated honestly rather than replaced by a guess — it
//! is the signal every consumer uses to stay quiet.

use crate::db::Db;
use crate::members::{members_of, FoundMethod, LookupOptions, Members};
use crate::name::{self, Name};
use crate::outline;
use crate::overload;
use crate::scope::{Context, Resolved};
use crate::symbols::ClassRef;
use crate::types::{Primitive, Substitution, Type};
use crate::MAX_EXPRESSION_DEPTH;
use java_syntax::{ast, Node};
use std::cell::{Cell, RefCell};

/// What a qualifier in front of a `.` denotes, which decides what may follow.
#[derive(Debug, Clone)]
pub enum Receiver {
    /// An expression: its instance members are accessible.
    Value(Type),
    /// A type name: its static members, and its nested types.
    StaticClass(Name),
    /// A package: its subpackages and classes.
    Package(String),
    /// `super`: members of the superclass, including overridden ones.
    Super(Type),
}

impl Receiver {
    pub fn value_type(&self) -> Option<&Type> {
        match self {
            Receiver::Value(ty) | Receiver::Super(ty) => Some(ty),
            _ => None,
        }
    }
}

pub struct Inferer<'a> {
    pub ctx: &'a Context<'a>,
    /// Recursion depth, as a backstop against any path that recurses further
    /// than an expression can nest.
    depth: Cell<u32>,
    /// Invocations currently being resolved, by syntax node id.
    ///
    /// A call's type can depend on its arguments, and an argument's type can
    /// depend on the call (that is how a lambda or `String[]::new` gets its
    /// target type). That mutual dependency is a genuine cycle, and a depth
    /// counter alone does not break it, because each turn of the cycle starts
    /// a fresh inference. Refusing to re-enter a call already in progress cuts
    /// it exactly, and the honest answer for the argument is then "unknown".
    active_calls: RefCell<Vec<usize>>,
}

/// Restores the recursion depth when it goes out of scope, including on an
/// early return.
struct DepthGuard<'a>(&'a Cell<u32>);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

/// Removes a call from the in-progress set when it goes out of scope.
struct CallGuard<'a>(&'a RefCell<Vec<usize>>);

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().pop();
    }
}

impl<'a> Inferer<'a> {
    pub fn new(ctx: &'a Context<'a>) -> Inferer<'a> {
        Inferer {
            ctx,
            depth: Cell::new(0),
            active_calls: RefCell::new(Vec::new()),
        }
    }

    fn enter(&self) -> Option<DepthGuard<'_>> {
        if self.depth.get() >= MAX_EXPRESSION_DEPTH {
            return None;
        }
        self.depth.set(self.depth.get() + 1);
        Some(DepthGuard(&self.depth))
    }

    /// Marks an invocation as being resolved, or refuses if it already is.
    fn enter_call(&self, node: Node<'_>) -> Option<CallGuard<'_>> {
        let mut active = self.active_calls.borrow_mut();
        if active.contains(&node.id()) {
            return None;
        }
        active.push(node.id());
        drop(active);
        Some(CallGuard(&self.active_calls))
    }

    fn db(&self) -> &Db {
        self.ctx.db
    }

    fn text(&self) -> &str {
        self.ctx.text()
    }

    /// The type of an expression node.
    pub fn type_of(&self, node: Node<'_>) -> Type {
        self.infer(node, 0)
    }

    fn infer(&self, node: Node<'_>, depth: u32) -> Type {
        if depth > MAX_EXPRESSION_DEPTH {
            return Type::Unknown;
        }
        let Some(_guard) = self.enter() else {
            return Type::Unknown;
        };
        match node.kind() {
            "decimal_integer_literal"
            | "hex_integer_literal"
            | "octal_integer_literal"
            | "binary_integer_literal" => {
                let text = ast::text(node, self.text());
                if text.ends_with('l') || text.ends_with('L') {
                    Type::Primitive(Primitive::Long)
                } else {
                    Type::INT
                }
            }
            "decimal_floating_point_literal" | "hex_floating_point_literal" => {
                let text = ast::text(node, self.text());
                if text.ends_with('f') || text.ends_with('F') {
                    Type::Primitive(Primitive::Float)
                } else {
                    Type::Primitive(Primitive::Double)
                }
            }
            "true" | "false" => Type::BOOLEAN,
            "character_literal" => Type::Primitive(Primitive::Char),
            "string_literal" | "text_block" => Type::string(),
            "null_literal" => Type::Null,
            "class_literal" => {
                // `Foo.class` has type Class<Foo>.
                let argument = ast::named_children(node)
                    .next()
                    .map(|ty| self.ctx.type_scope().lower_type(ty, self.text()))
                    .unwrap_or(Type::Unknown);
                Type::generic("java.lang.Class", vec![argument])
            }
            "this" => self.ctx.self_type.clone().unwrap_or(Type::Unknown),
            "super" => self.super_type(),
            "identifier" => match self.ctx.resolve_name(ast::text(node, self.text())) {
                Some(resolved) => resolved.value_type().unwrap_or(Type::Unknown),
                None => Type::Unknown,
            },
            "parenthesized_expression" => ast::named_children(node)
                .next()
                .map(|inner| self.infer(inner, depth + 1))
                .unwrap_or(Type::Unknown),
            "field_access" => self.field_access_type(node, depth),
            "array_access" => {
                let array = ast::field(node, "array")
                    .map(|a| self.infer(a, depth + 1))
                    .unwrap_or(Type::Unknown);
                match array {
                    Type::Array(element) => *element,
                    _ => Type::Unknown,
                }
            }
            "method_invocation" => match self.resolve_invocation(node) {
                Some(found) => found.method.ret,
                None => Type::Unknown,
            },
            "object_creation_expression" => self.creation_type(node, depth),
            "array_creation_expression" => {
                let element = ast::field(node, "type")
                    .map(|ty| self.ctx.type_scope().lower_type(ty, self.text()))
                    .unwrap_or(Type::Unknown);
                let dimensions = ast::named_children(node)
                    .filter(|c| matches!(c.kind(), "dimensions_expr" | "dimensions"))
                    .map(|d| ast::text(d, self.text()).matches('[').count())
                    .sum::<usize>()
                    .max(1);
                (0..dimensions).fold(element, |inner, _| Type::Array(Box::new(inner)))
            }
            "array_initializer" => Type::Unknown,
            "cast_expression" => ast::field(node, "type")
                .map(|ty| self.ctx.type_scope().lower_type(ty, self.text()))
                .unwrap_or(Type::Unknown),
            "instanceof_expression" => Type::BOOLEAN,
            "binary_expression" => self.binary_type(node, depth),
            "unary_expression" => {
                let operator = ast::field(node, "operator")
                    .map(|o| ast::text(o, self.text()).to_string())
                    .unwrap_or_default();
                if operator == "!" {
                    return Type::BOOLEAN;
                }
                let operand = ast::field(node, "operand")
                    .map(|o| self.infer(o, depth + 1))
                    .unwrap_or(Type::Unknown);
                // Unary numeric promotion: byte, short, and char become int.
                match self.numeric(&operand) {
                    Some(p) if p.is_integral() && p != Primitive::Long => Type::INT,
                    Some(p) => Type::Primitive(p),
                    None => operand,
                }
            }
            "update_expression" => ast::named_children(node)
                .next()
                .map(|operand| self.infer(operand, depth + 1))
                .unwrap_or(Type::Unknown),
            "assignment_expression" => ast::field(node, "left")
                .map(|left| self.infer(left, depth + 1))
                .unwrap_or(Type::Unknown),
            "ternary_expression" => {
                let left = ast::field(node, "consequence")
                    .map(|c| self.infer(c, depth + 1))
                    .unwrap_or(Type::Unknown);
                if !left.is_unknown() && !matches!(left, Type::Null) {
                    return left;
                }
                ast::field(node, "alternative")
                    .map(|a| self.infer(a, depth + 1))
                    .unwrap_or(left)
            }
            "lambda_expression" | "method_reference" => {
                // A lambda's type is whatever functional interface it is being
                // assigned to; with no target in sight there is nothing to say.
                self.target_type(node).unwrap_or(Type::Unknown)
            }
            "switch_expression" => Type::Unknown,
            "scoped_type_identifier" | "scoped_identifier" => {
                // Appears where error recovery produced a dotted name; treat
                // it as a field access chain.
                self.dotted_value_type(node)
            }
            "type_identifier" => Type::Unknown,
            _ => Type::Unknown,
        }
    }

    fn numeric(&self, ty: &Type) -> Option<Primitive> {
        match ty {
            Type::Primitive(p) if p.is_numeric() => Some(*p),
            Type::Class { name, .. } => Primitive::unboxed(name).filter(|p| p.is_numeric()),
            _ => None,
        }
    }

    fn binary_type(&self, node: Node<'_>, depth: u32) -> Type {
        let operator = ast::field(node, "operator")
            .map(|o| ast::text(o, self.text()).to_string())
            .unwrap_or_default();
        match operator.as_str() {
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => return Type::BOOLEAN,
            _ => {}
        }
        let left = ast::field(node, "left")
            .map(|l| self.infer(l, depth + 1))
            .unwrap_or(Type::Unknown);
        let right = ast::field(node, "right")
            .map(|r| self.infer(r, depth + 1))
            .unwrap_or(Type::Unknown);
        if operator == "+" {
            // String concatenation wins over numeric promotion.
            let is_string = |ty: &Type| ty.class_name().is_some_and(|n| &**n == "java.lang.String");
            if is_string(&left) || is_string(&right) {
                return Type::string();
            }
        }
        if matches!(operator.as_str(), "<<" | ">>" | ">>>") {
            // Shift results take the promoted type of the left operand only.
            return match self.numeric(&left) {
                Some(Primitive::Long) => Type::Primitive(Primitive::Long),
                Some(_) => Type::INT,
                None => Type::Unknown,
            };
        }
        // The bitwise operators are also the non-short-circuit boolean ones.
        let either_is_boolean = matches!(left, Type::Primitive(Primitive::Boolean))
            || matches!(right, Type::Primitive(Primitive::Boolean));
        if matches!(operator.as_str(), "&" | "|" | "^") && either_is_boolean {
            return Type::BOOLEAN;
        }
        match (self.numeric(&left), self.numeric(&right)) {
            (Some(a), Some(b)) => Type::Primitive(a.promote(b)),
            _ => Type::Unknown,
        }
    }

    fn super_type(&self) -> Type {
        let Some(class) = &self.ctx.class else {
            return Type::Unknown;
        };
        match &class.superclass {
            Some(superclass) => superclass.clone(),
            // An interface's `super` is only meaningful qualified; Object is
            // the honest answer for a class without an explicit superclass.
            None => Type::object(),
        }
    }

    fn field_access_type(&self, node: Node<'_>, depth: u32) -> Type {
        let Some(field_node) = ast::field(node, "field") else {
            return Type::Unknown;
        };
        let field_name = ast::text(field_node, self.text());
        let Some(object) = ast::field(node, "object") else {
            return Type::Unknown;
        };
        match self.receiver_of(object, depth) {
            Some(Receiver::Value(ty)) | Some(Receiver::Super(ty)) => {
                if let Type::Array(_) = ty {
                    if field_name == "length" {
                        return Type::INT;
                    }
                }
                // `Outer.this` inside an inner class.
                if field_name == "this" {
                    return ty;
                }
                let members = self.members_for(&ty, LookupOptions::instance());
                members
                    .field(field_name)
                    .map(|found| found.field.ty.clone())
                    .unwrap_or(Type::Unknown)
            }
            Some(Receiver::StaticClass(class)) => {
                if field_name == "class" {
                    return Type::generic("java.lang.Class", vec![Type::class(class)]);
                }
                if field_name == "this" {
                    return Type::class(class);
                }
                let members =
                    members_of(self.db(), &Type::class(class), self.static_lookup_options());
                members
                    .field(field_name)
                    .map(|found| found.field.ty.clone())
                    .unwrap_or(Type::Unknown)
            }
            // `java.util.List` as a value has no type; it is a type name.
            Some(Receiver::Package(_)) | None => Type::Unknown,
        }
    }

    /// Visibility options for looking up members on the receiver: private
    /// members are visible throughout the declaring top-level class.
    fn lookup_options_for(&self, ty: &Type, base: LookupOptions) -> LookupOptions {
        let same_class = ty
            .class_name()
            .zip(self.ctx.enclosing.first())
            .map(|(a, b)| a.split('$').next() == b.split('$').next())
            .unwrap_or(false);
        LookupOptions {
            include_private: same_class,
            ..base
        }
    }

    fn static_lookup_options(&self) -> LookupOptions {
        LookupOptions {
            include_instance: false,
            ..LookupOptions::default()
        }
    }

    pub fn members_for(&self, ty: &Type, base: LookupOptions) -> Members {
        members_of(self.db(), ty, self.lookup_options_for(ty, base))
    }

    /// Classifies the expression to the left of a `.`.
    pub fn receiver_of(&self, node: Node<'_>, depth: u32) -> Option<Receiver> {
        match node.kind() {
            "identifier" | "type_identifier" => {
                let written = ast::text(node, self.text());
                match self.ctx.resolve_name(written)? {
                    Resolved::Local(local) => Some(Receiver::Value(local.ty)),
                    Resolved::Field { field, .. } => Some(Receiver::Value(field.ty)),
                    Resolved::Class(binary) => Some(Receiver::StaticClass(binary)),
                    Resolved::TypeVar(var) => Some(Receiver::Value(Type::Var(Box::new(var)))),
                    Resolved::Package(package) => Some(Receiver::Package(package)),
                }
            }
            "super" => Some(Receiver::Super(self.super_type())),
            "this" => Some(Receiver::Value(self.ctx.self_type.clone()?)),
            "field_access" | "scoped_type_identifier" | "scoped_identifier" => {
                // A dotted qualifier may be a package, a type, or a value.
                let written = outline::dotted(node, self.text());
                if let Some(receiver) = self.classify_written(&written) {
                    return Some(receiver);
                }
                let ty = self.infer(node, depth + 1);
                (!ty.is_unknown()).then_some(Receiver::Value(ty))
            }
            _ => {
                let ty = self.infer(node, depth + 1);
                (!ty.is_unknown()).then_some(Receiver::Value(ty))
            }
        }
    }

    /// Classifies a written dotted name as a package or a type, if it is one.
    pub fn classify_written(&self, written: &str) -> Option<Receiver> {
        if written.is_empty() {
            return None;
        }
        // A leading simple name that is a local or field makes this a value
        // expression, not a type name.
        let head = written.split('.').next().unwrap_or(written);
        if self.ctx.local(head).is_some() {
            return None;
        }
        if let Some(binary) = self.ctx.type_scope().resolve_class_name(written) {
            return Some(Receiver::StaticClass(binary));
        }
        if self.db().is_package(written) {
            return Some(Receiver::Package(written.to_string()));
        }
        None
    }

    /// The type of a dotted chain that error recovery left as one node.
    fn dotted_value_type(&self, node: Node<'_>) -> Type {
        let written = outline::dotted(node, self.text());
        let mut parts = written.split('.');
        let Some(head) = parts.next() else {
            return Type::Unknown;
        };
        let mut current = match self.ctx.resolve_name(head) {
            Some(Resolved::Local(local)) => local.ty,
            Some(Resolved::Field { field, .. }) => field.ty,
            Some(Resolved::Class(binary)) => {
                // Walk the remaining segments as static then instance fields.
                let mut owner = Type::class(binary);
                for segment in parts {
                    let members = members_of(self.db(), &owner, LookupOptions::default());
                    match members.field(segment) {
                        Some(found) => owner = found.field.ty.clone(),
                        None => return Type::Unknown,
                    }
                }
                return owner;
            }
            _ => return Type::Unknown,
        };
        for segment in parts {
            let members = self.members_for(&current, LookupOptions::instance());
            match members.field(segment) {
                Some(found) => current = found.field.ty.clone(),
                None => return Type::Unknown,
            }
        }
        current
    }

    fn creation_type(&self, node: Node<'_>, depth: u32) -> Type {
        let Some(type_node) = ast::field(node, "type") else {
            return Type::Unknown;
        };
        let created = self.ctx.type_scope().lower_type(type_node, self.text());
        // `new ArrayList<>()` takes its type arguments from the target type.
        let is_diamond = matches!(&created, Type::Class { args, .. } if args.is_empty())
            && ast::named_children(type_node)
                .any(|c| c.kind() == "type_arguments" && ast::named_children(c).count() == 0);
        if is_diamond {
            if let (Some(target), Some(name)) = (self.target_type(node), created.class_name()) {
                if target.class_name().is_some() && !target.type_args().is_empty() {
                    // Only adopt the target's arguments when the arity matches
                    // the created class's own parameters.
                    if let Some(class) = self.db().class(name) {
                        if class.type_params.len() == target.type_args().len() {
                            return Type::Class {
                                name: name.clone(),
                                args: target.type_args().to_vec(),
                            };
                        }
                    }
                }
            }
        }
        let _ = depth;
        created
    }

    /// The type an expression is expected to have, from its surroundings.
    /// Used for diamond inference and for lambdas.
    pub fn target_type(&self, node: Node<'_>) -> Option<Type> {
        let _depth = self.enter()?;
        let parent = node.parent()?;
        match parent.kind() {
            "variable_declarator" => {
                let declaration = parent.parent()?;
                let declared = ast::field(declaration, "type")?;
                Some(self.ctx.type_scope().lower_type(declared, self.text()))
            }
            "assignment_expression" => {
                let left = ast::field(parent, "left")?;
                Some(self.infer(left, 0))
            }
            "return_statement" => self.enclosing_return_type(parent),
            "argument_list" => {
                // The parameter type at this argument's position.
                let call = parent.parent()?;
                let index =
                    ast::named_children(parent).position(|argument| argument.id() == node.id())?;
                let found = self.resolve_invocation(call)?;
                found.method.params.get(index).map(|p| p.ty.clone())
            }
            "cast_expression" => {
                let declared = ast::field(parent, "type")?;
                Some(self.ctx.type_scope().lower_type(declared, self.text()))
            }
            "parenthesized_expression" => self.target_type(parent),
            _ => None,
        }
    }

    fn enclosing_return_type(&self, node: Node<'_>) -> Option<Type> {
        for ancestor in ast::ancestors(node) {
            match ancestor.kind() {
                "lambda_expression" => {
                    // A lambda's return type comes from its functional
                    // interface, which is more than this layer tracks.
                    return None;
                }
                "method_declaration" => {
                    let declared = ast::field(ancestor, "type")?;
                    return Some(self.ctx.type_scope().lower_type(declared, self.text()));
                }
                _ => {}
            }
        }
        None
    }

    /// Resolves a `method_invocation` or `object_creation_expression` to the
    /// declaration it calls, with type arguments applied to the signature.
    pub fn resolve_invocation(&self, node: Node<'_>) -> Option<FoundMethod> {
        let _depth = self.enter()?;
        let _call = self.enter_call(node)?;
        match node.kind() {
            "method_invocation" => self.resolve_method_call(node),
            "object_creation_expression" => self.resolve_constructor_call(node),
            "explicit_constructor_invocation" => self.resolve_explicit_constructor(node),
            _ => None,
        }
    }

    fn argument_types(&self, node: Node<'_>) -> Vec<Type> {
        let Some(list) = ast::field(node, "arguments") else {
            return Vec::new();
        };
        ast::named_children(list)
            .map(|argument| self.infer(argument, 1))
            .collect()
    }

    /// Candidate methods for a call, before overload resolution.
    pub fn call_candidates(&self, node: Node<'_>) -> Option<(Name, Vec<FoundMethod>)> {
        let name_node = ast::field(node, "name")?;
        let called = name::name(ast::text(name_node, self.text()));
        let members = match ast::field(node, "object") {
            Some(object) => match self.receiver_of(object, 0)? {
                Receiver::Value(ty) => self.members_for(&ty, LookupOptions::instance()),
                Receiver::Super(ty) => self.members_for(&ty, LookupOptions::default()),
                Receiver::StaticClass(binary) => members_of(
                    self.db(),
                    &Type::class(binary),
                    self.static_lookup_options(),
                ),
                Receiver::Package(_) => return None,
            },
            None => self.unqualified_members(&called),
        };
        let candidates: Vec<FoundMethod> = members
            .methods
            .into_iter()
            .filter(|found| found.method.name == called)
            .collect();
        Some((called, candidates))
    }

    /// Members reachable from an unqualified call: the enclosing classes'
    /// members, then anything brought in by a static import.
    fn unqualified_members(&self, called: &str) -> Members {
        let mut members = Members::default();
        for (depth, enclosing) in self.ctx.enclosing.iter().enumerate() {
            let Some(class) = self.db().class(enclosing) else {
                continue;
            };
            let self_type = crate::lower::self_type_of(&class);
            let found = members_of(
                self.db(),
                &self_type,
                LookupOptions {
                    include_private: true,
                    include_instance: !self.ctx.in_static_context || depth > 0,
                    ..LookupOptions::default()
                },
            );
            members.methods.extend(found.methods);
            members.fields.extend(found.fields);
        }
        if members.methods.iter().any(|m| &*m.method.name == called) {
            return members;
        }
        for import in &self.ctx.outline.imports {
            if !import.is_static {
                continue;
            }
            let (owner, member) = match import.is_wildcard {
                true => (import.path.to_string(), called),
                false => {
                    if import.simple_name() != called {
                        continue;
                    }
                    let Some(dot) = import.path.rfind('.') else {
                        continue;
                    };
                    (import.path[..dot].to_string(), &import.path[dot + 1..])
                }
            };
            let scope = self.ctx.type_scope();
            let Some(class) = scope.resolve_class(&owner) else {
                continue;
            };
            let found = members_of(
                self.db(),
                &Type::class(class.name.clone()),
                self.static_lookup_options(),
            );
            members.methods.extend(
                found
                    .methods
                    .into_iter()
                    .filter(|m| &*m.method.name == member),
            );
        }
        members
    }

    fn resolve_method_call(&self, node: Node<'_>) -> Option<FoundMethod> {
        let (_, candidates) = self.call_candidates(node)?;
        if candidates.is_empty() {
            return None;
        }
        let args = self.argument_types(node);
        let by_arity: Vec<&FoundMethod> = candidates
            .iter()
            .filter(|found| found.method.accepts_arity(args.len()))
            .collect();
        let chosen = overload::select(self.db(), &by_arity, &args)
            // No applicable overload: fall back to the only arity match so
            // hover and signature help still have something true to show.
            .or_else(|| by_arity.first().copied())
            .or_else(|| candidates.first())?;
        Some(self.apply_type_arguments(chosen.clone(), node, &args))
    }

    /// Applies explicit or inferred method type arguments to a signature.
    fn apply_type_arguments(
        &self,
        mut found: FoundMethod,
        node: Node<'_>,
        args: &[Type],
    ) -> FoundMethod {
        if found.method.type_params.is_empty() {
            return found;
        }
        let explicit: Vec<Type> = ast::named_children(node)
            .find(|c| c.kind() == "type_arguments")
            .map(|list| {
                ast::named_children(list)
                    .map(|ty| self.ctx.type_scope().lower_type(ty, self.text()))
                    .collect()
            })
            .unwrap_or_default();
        let subst = if explicit.len() == found.method.type_params.len() {
            let mut subst = Substitution::new();
            for (param, argument) in found.method.type_params.iter().zip(explicit) {
                subst.insert(param.name.clone(), argument);
            }
            subst
        } else {
            overload::infer_type_arguments(&found.method, args)
        };
        found.method.ret = found.method.ret.substitute(&subst);
        for param in &mut found.method.params {
            param.ty = param.ty.substitute(&subst);
        }
        found
    }

    fn resolve_constructor_call(&self, node: Node<'_>) -> Option<FoundMethod> {
        let created = self.creation_type(node, 0);
        let class = self.db().class(created.class_name()?)?;
        let args = self.argument_types(node);
        self.select_constructor(&class, &created, &args)
    }

    fn resolve_explicit_constructor(&self, node: Node<'_>) -> Option<FoundMethod> {
        let text = self.text();
        let is_super = ast::named_children(node).any(|c| ast::text(c, text) == "super");
        let target = if is_super {
            self.super_type()
        } else {
            self.ctx.self_type.clone()?
        };
        let class = self.db().class(target.class_name()?)?;
        let args = self.argument_types(node);
        self.select_constructor(&class, &target, &args)
    }

    fn select_constructor(
        &self,
        class: &ClassRef,
        created: &Type,
        args: &[Type],
    ) -> Option<FoundMethod> {
        let subst = class.substitution_for(created.type_args());
        let candidates: Vec<FoundMethod> = class
            .methods
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.is_constructor)
            .map(|(position, constructor)| {
                let mut method = constructor.clone();
                for param in &mut method.params {
                    param.ty = param.ty.substitute(&subst);
                }
                method.ret = created.clone();
                FoundMethod {
                    owner: class.clone(),
                    method,
                    depth: 0,
                    index: position as u32,
                }
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let by_arity: Vec<&FoundMethod> = candidates
            .iter()
            .filter(|found| found.method.accepts_arity(args.len()))
            .collect();
        let chosen = overload::select(self.db(), &by_arity, args)
            .or_else(|| by_arity.first().copied())
            .or_else(|| candidates.first())?;
        Some(chosen.clone())
    }

    /// All constructors of a class, for signature help on `new Foo(`.
    pub fn constructors_of(&self, created: &Type) -> Vec<FoundMethod> {
        let Some(class) = created.class_name().and_then(|n| self.db().class(n)) else {
            return Vec::new();
        };
        let subst = class.substitution_for(created.type_args());
        class
            .methods
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.is_constructor)
            .map(|(position, constructor)| {
                let mut method = constructor.clone();
                for param in &mut method.params {
                    param.ty = param.ty.substitute(&subst);
                }
                FoundMethod {
                    owner: class.clone(),
                    method,
                    depth: 0,
                    index: position as u32,
                }
            })
            .collect()
    }
}

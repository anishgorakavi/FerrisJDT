//! Expression type inference.
//!
//! Types are computed on demand from the syntax tree; nothing is stored on
//! nodes. An expression whose type cannot be determined yields `Type::Unknown`,
//! and that value is propagated honestly rather than replaced by a guess — it
//! is the signal every consumer uses to stay quiet.

use crate::db::Db;
use crate::members::{
    functional_interface_method, is_subtype, members_of, FoundMethod, LookupOptions, Members,
};
use crate::name::{self, Name};
use crate::outline;
use crate::overload;
use crate::scope::{self, Context, Local, Resolved};
use crate::solve::{self, Inference};
use crate::symbols::{ClassRef, MethodData};
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
            "method_invocation" => match self.array_clone(node, depth) {
                Some(array) => array,
                None => match self.resolve_invocation(node) {
                    Some(found) => found.method.ret,
                    None => Type::Unknown,
                },
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

    fn creation_type(&self, node: Node<'_>, _depth: u32) -> Type {
        self.creation_type_expecting(node, None)
    }

    /// The type a `new` expression creates. A diamond takes its arguments from
    /// the type the expression is expected to have, and failing that from the
    /// constructor arguments: `new Box<>("s")` is a `Box<String>`.
    fn creation_type_expecting(&self, node: Node<'_>, expected: Option<&Type>) -> Type {
        let Some(type_node) = ast::field(node, "type") else {
            return Type::Unknown;
        };
        // `outer.new Inner()` names `Inner` as a member of the outer instance's class.
        if let Some(qualifier) = ast::creation_qualifier(node) {
            let outer = self.type_of(qualifier);
            let simple = ast::named_children(type_node)
                .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
                .map(|c| ast::text(c, self.text()))
                .unwrap_or_else(|| ast::text(type_node, self.text()));
            return match outer.class_name() {
                Some(outer_name) => {
                    let binary = format!("{outer_name}${simple}");
                    if self.db().has_class(&binary) {
                        Type::class(binary)
                    } else {
                        Type::Unknown
                    }
                }
                None => Type::Unknown,
            };
        }
        let created = self.ctx.type_scope().lower_type(type_node, self.text());
        let is_diamond = matches!(&created, Type::Class { args, .. } if args.is_empty())
            && ast::named_children(type_node)
                .any(|c| c.kind() == "type_arguments" && ast::named_children(c).count() == 0);
        if !is_diamond {
            return created;
        }
        let Some(class) = created.class_name().and_then(|name| self.db().class(name)) else {
            return created;
        };
        if class.type_params.is_empty() {
            return created;
        }
        let target = expected.cloned().or_else(|| self.target_type(node));
        if let Some(target) = &target {
            // Only adopt the target's arguments when the arity matches the
            // created class's own parameters, and they are fully known.
            if target.class_name().is_some()
                && !target.type_args().is_empty()
                && !target.has_unknown()
                && class.type_params.len() == target.type_args().len()
            {
                return Type::Class {
                    name: class.name.clone(),
                    args: target.type_args().to_vec(),
                };
            }
        }
        self.diamond_from_arguments(node, &class, target.as_ref())
            .unwrap_or(created)
    }

    /// Solves a diamond's type arguments from the constructor arguments, the
    /// class's own parameters being the variables of a generic constructor.
    fn diamond_from_arguments(
        &self,
        node: Node<'_>,
        class: &ClassRef,
        target: Option<&Type>,
    ) -> Option<Type> {
        let args = self.argument_types(node);
        let self_type = crate::lower::self_type_of(class);
        let candidates: Vec<FoundMethod> = class
            .methods
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.is_constructor)
            .map(|(position, constructor)| {
                let mut method = constructor.clone();
                let mut variables = class.type_params.clone();
                variables.extend(method.type_params.iter().cloned());
                method.type_params = variables;
                method.ret = self_type.clone();
                FoundMethod {
                    owner: class.clone(),
                    method,
                    depth: 0,
                    index: position as u32,
                }
            })
            .collect();
        let by_arity: Vec<&FoundMethod> = candidates
            .iter()
            .filter(|found| found.method.accepts_arity(args.len()))
            .collect();
        let chosen =
            overload::select(self.db(), &by_arity, &args).or_else(|| by_arity.first().copied())?;
        let mut inference = Inference::new(self.db(), &chosen.method.type_params);
        let spread = solve::spreads_varargs(&chosen.method, &args);
        solve::bind_arguments(&mut inference, &chosen.method, &args, spread);
        if let Some(target) = target {
            inference.bind_expected(&self_type, target);
        }
        Some(self_type.substitute(&inference.finish()))
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

    /// `array.clone()` returns the array's own type, not `Object` (JLS 10.7).
    fn array_clone(&self, node: Node<'_>, depth: u32) -> Option<Type> {
        let name = ast::field(node, "name")?;
        if ast::text(name, self.text()) != "clone" {
            return None;
        }
        if ast::field(node, "arguments").is_some_and(|list| ast::named_children(list).count() > 0) {
            return None;
        }
        let receiver = self.infer(ast::field(node, "object")?, depth + 1);
        matches!(receiver, Type::Array(_)).then_some(receiver)
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
            let Some(class) = self.db().class_declared_in(self.ctx.file, enclosing) else {
                continue;
            };
            let self_type = crate::lower::self_type_of(&class);
            let found = crate::members::members_of_declared(
                self.db(),
                &self_type,
                LookupOptions {
                    include_private: true,
                    include_instance: !self.ctx.in_static_context || depth > 0,
                    ..LookupOptions::default()
                },
                Some(&class),
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
        self.resolve_method_call_expecting(node, None)
    }

    /// Resolves a call. `expected` is the type the call must produce, which
    /// can solve variables its arguments leave open (`Collectors.toList()`).
    fn resolve_method_call_expecting(
        &self,
        node: Node<'_>,
        expected: Option<&Type>,
    ) -> Option<FoundMethod> {
        let (_, candidates) = self.call_candidates(node)?;
        if candidates.is_empty() {
            return None;
        }
        let args = self.argument_types(node);
        let by_arity: Vec<&FoundMethod> = candidates
            .iter()
            .filter(|found| found.method.accepts_arity(args.len()))
            .collect();
        let by_arity = self.narrow_by_lambda_arity(node, by_arity);
        let chosen = overload::select(self.db(), &by_arity, &args)
            // No applicable overload: fall back to the only arity match so
            // hover and signature help still have something true to show.
            .or_else(|| by_arity.first().copied())
            .or_else(|| candidates.first())?;
        Some(self.apply_type_arguments(chosen.clone(), node, &args, expected))
    }

    /// Drops overloads whose functional-interface parameter takes a different
    /// number of arguments than the lambda written there: `thenComparing(e -> ...)`
    /// is the `Function` overload, not the `Comparator` one.
    pub fn narrow_by_lambda_arity<'m>(
        &self,
        node: Node<'_>,
        candidates: Vec<&'m FoundMethod>,
    ) -> Vec<&'m FoundMethod> {
        if candidates.len() < 2 {
            return candidates;
        }
        let Some(list) = ast::field(node, "arguments") else {
            return candidates;
        };
        let lambdas: Vec<(usize, usize)> = ast::named_children(list)
            .enumerate()
            .filter(|(_, argument)| argument.kind() == "lambda_expression")
            .map(|(index, lambda)| {
                (
                    index,
                    scope::lambda_parameter_names(lambda, self.text()).len(),
                )
            })
            .collect();
        if lambdas.is_empty() {
            return candidates;
        }
        let narrowed: Vec<&FoundMethod> = candidates
            .iter()
            .copied()
            .filter(|found| {
                lambdas.iter().all(|(index, arity)| {
                    let Some(param) = found.method.params.get(*index) else {
                        return true;
                    };
                    match functional_interface_method(self.db(), &param.ty) {
                        Some(function) => function.method.params.len() == *arity,
                        None => true,
                    }
                })
            })
            .collect();
        if narrowed.is_empty() {
            candidates
        } else {
            narrowed
        }
    }

    /// Applies explicit or inferred method type arguments to a signature.
    fn apply_type_arguments(
        &self,
        mut found: FoundMethod,
        node: Node<'_>,
        args: &[Type],
        expected: Option<&Type>,
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
            self.solve_call(&found.method, node, args, expected)
        };
        found.method.ret = found.method.ret.substitute(&subst);
        for param in &mut found.method.params {
            param.ty = param.ty.substitute(&subst);
        }
        found
    }

    /// Solves a generic method's type variables for one call, in the order
    /// that lets each step use the last: typed arguments, then the expected
    /// type, then nested calls against the parameter they fill, then the
    /// results of lambdas and method references.
    fn solve_call(
        &self,
        method: &MethodData,
        node: Node<'_>,
        args: &[Type],
        expected: Option<&Type>,
    ) -> Substitution {
        let mut inference = Inference::new(self.db(), &method.type_params);
        let spread = solve::spreads_varargs(method, args);
        solve::bind_arguments(&mut inference, method, args, spread);

        let argument_nodes: Vec<Node<'_>> = ast::field(node, "arguments")
            .map(|list| ast::named_children(list).collect())
            .unwrap_or_default();
        let has_open_variable = |inference: &Inference<'_>| {
            method
                .type_params
                .iter()
                .any(|param| !inference.is_solved(&param.name))
        };

        let target;
        let expected = match expected {
            Some(expected) => Some(expected),
            None if has_open_variable(&inference) => {
                target = self.target_type(node);
                target.as_ref()
            }
            None => None,
        };
        if let Some(expected) = expected {
            inference.bind_expected(&method.ret, expected);
        }

        // A nested call whose type depended on a context it did not have is
        // inferred again against the parameter it fills.
        for (index, argument) in argument_nodes.iter().enumerate() {
            if !matches!(
                argument.kind(),
                "method_invocation" | "object_creation_expression"
            ) || !args.get(index).is_some_and(Type::has_unknown)
            {
                continue;
            }
            let Some(param) = param_for_argument(method, index, spread) else {
                continue;
            };
            let wanted = param.substitute(&inference.open_substitution());
            let refined = self.infer_expecting(*argument, &wanted);
            if !refined.is_unknown() {
                inference.bind(&param, &refined);
            }
        }

        // Twice, so that a lambda's result can feed the parameter of the next.
        for _ in 0..2 {
            for (index, argument) in argument_nodes.iter().enumerate() {
                if matches!(argument.kind(), "lambda_expression" | "method_reference") {
                    self.bind_from_function(&mut inference, method, spread, index, *argument);
                }
            }
        }
        inference.finish()
    }

    /// Infers an expression that has a type it is expected to fit.
    fn infer_expecting(&self, node: Node<'_>, expected: &Type) -> Type {
        match node.kind() {
            "method_invocation" => {
                let Some(_depth) = self.enter() else {
                    return Type::Unknown;
                };
                let Some(_call) = self.enter_call(node) else {
                    return Type::Unknown;
                };
                self.resolve_method_call_expecting(node, Some(expected))
                    .map(|found| found.method.ret)
                    .unwrap_or(Type::Unknown)
            }
            "object_creation_expression" => self.creation_type_expecting(node, Some(expected)),
            _ => self.type_of(node),
        }
    }

    /// Binds a function-typed parameter's return variable from what the lambda
    /// or method reference given for it returns.
    fn bind_from_function(
        &self,
        inference: &mut Inference<'_>,
        method: &MethodData,
        spread: bool,
        index: usize,
        argument: Node<'_>,
    ) {
        let Some(param) = param_for_argument(method, index, spread) else {
            return;
        };
        let Some(declared) = functional_interface_method(self.db(), &param) else {
            return;
        };
        let text = self.text();

        // A lambda that writes its parameter types binds the interface's directly.
        if argument.kind() == "lambda_expression" {
            if let Some(parameters) = ast::field(argument, "parameters") {
                if parameters.kind() == "formal_parameters" {
                    for (formal, declared_param) in
                        ast::named_children(parameters).zip(&declared.method.params)
                    {
                        if let Some(type_node) = ast::field(formal, "type") {
                            let written = self.ctx.type_scope().lower_type(type_node, text);
                            inference.bind(&declared_param.ty, &written);
                        }
                    }
                }
            }
        }

        let current = param.substitute(inference.solved());
        let Some(function) = functional_interface_method(self.db(), &current) else {
            return;
        };
        let inputs: Vec<Type> = function
            .method
            .params
            .iter()
            .map(|p| scope::lambda_parameter_type(&p.ty))
            .collect();
        if inputs
            .iter()
            .any(|ty| inference.mentions_unsolved(ty) || ty.has_unknown())
        {
            return;
        }
        let result = if argument.kind() == "lambda_expression" {
            if scope::lambda_parameter_names(argument, text).len() != inputs.len() {
                return;
            }
            self.lambda_result_type(argument, &inputs)
        } else {
            self.method_reference_result(argument, &inputs)
        };
        if !result.is_unknown() && !result.is_void() {
            inference.bind(&declared.method.ret, &result);
        }
    }

    /// Runs `f` with an inferer in which extra locals are in scope, sharing the
    /// recursion guards so that nesting cannot restart the depth count.
    fn with_locals<T>(&self, locals: Vec<Local>, f: impl FnOnce(&Inferer<'_>) -> T) -> T {
        let mut context = self.ctx.clone();
        context.locals.splice(0..0, locals);
        let inner = Inferer {
            ctx: &context,
            depth: Cell::new(self.depth.get()),
            active_calls: RefCell::new(self.active_calls.borrow().clone()),
        };
        f(&inner)
    }

    /// The type a lambda returns when its parameters have the given types.
    fn lambda_result_type(&self, lambda: Node<'_>, inputs: &[Type]) -> Type {
        let text = self.text();
        let Some(body) = ast::field(lambda, "body") else {
            return Type::Unknown;
        };
        let mut locals: Vec<Local> = scope::lambda_parameter_names(lambda, text)
            .into_iter()
            .zip(inputs)
            .map(|(name, ty)| Local {
                name: name::name(&name),
                ty: ty.clone(),
                name_range: (0, 0),
                is_parameter: true,
            })
            .collect();
        if body.kind() == "block" {
            let scope = self.ctx.type_scope();
            scope::collect_block_locals(body, u32::MAX, text, &scope, &mut locals);
            // Declarations come out in source order; the latest must win.
            locals.reverse();
        }
        self.with_locals(locals, |inner| {
            if body.kind() == "block" {
                inner.block_result_type(body)
            } else {
                inner.type_of(body)
            }
        })
    }

    /// The type of the first `return` in a block that has a known type,
    /// skipping returns that belong to nested lambdas and classes.
    fn block_result_type(&self, block: Node<'_>) -> Type {
        let mut pending = vec![block];
        while let Some(node) = pending.pop() {
            for child in ast::named_children(node) {
                match child.kind() {
                    "lambda_expression" | "class_body" | "class_declaration" => {}
                    "return_statement" => {
                        if let Some(value) = ast::named_children(child).next() {
                            let ty = self.type_of(value);
                            if !ty.is_unknown() && !matches!(ty, Type::Null) {
                                return ty;
                            }
                        }
                    }
                    _ => pending.push(child),
                }
            }
        }
        Type::Unknown
    }

    /// The type a method reference returns when called with the given argument types.
    fn method_reference_result(&self, node: Node<'_>, inputs: &[Type]) -> Type {
        let text = self.text();
        let Some(qualifier) = ast::named_children(node).next() else {
            return Type::Unknown;
        };
        let Some(last) = node.child(node.child_count().saturating_sub(1)) else {
            return Type::Unknown;
        };
        let written = ast::text(last, text);
        if written == "new" {
            return match qualifier.kind() {
                "array_type" | "generic_type" => self.ctx.type_scope().lower_type(qualifier, text),
                _ => match self.receiver_of(qualifier, 0) {
                    Some(Receiver::StaticClass(binary)) => Type::class(binary),
                    _ => Type::Unknown,
                },
            };
        }
        let called = name::name(written);
        match self.receiver_of(qualifier, 0) {
            Some(Receiver::StaticClass(binary)) => {
                self.class_reference_result(&binary, &called, inputs)
            }
            Some(Receiver::Value(ty)) | Some(Receiver::Super(ty)) => {
                let members = self.members_for(&ty, LookupOptions::instance());
                self.return_of_call(&members, &called, inputs)
            }
            _ => Type::Unknown,
        }
    }

    /// `Class::name`: a static method taking all the inputs, or an instance
    /// method on the first input taking the rest (`String::length`).
    fn class_reference_result(&self, binary: &Name, called: &str, inputs: &[Type]) -> Type {
        let class_type = Type::class(binary.clone());
        let statics = members_of(self.db(), &class_type, self.static_lookup_options());
        let result = self.return_of_call(&statics, called, inputs);
        if !result.is_unknown() {
            return result;
        }
        let Some((receiver, rest)) = inputs.split_first() else {
            return Type::Unknown;
        };
        let receiver_type = if is_subtype(self.db(), receiver, &class_type) {
            receiver.clone()
        } else {
            class_type
        };
        let members = self.members_for(&receiver_type, LookupOptions::instance());
        self.return_of_call(&members, called, rest)
    }

    /// The return type of the overload of `called` that accepts these arguments.
    fn return_of_call(&self, members: &Members, called: &str, args: &[Type]) -> Type {
        let candidates: Vec<&FoundMethod> = members
            .methods
            .iter()
            .filter(|found| &*found.method.name == called && found.method.accepts_arity(args.len()))
            .collect();
        let chosen = overload::select(self.db(), &candidates, args)
            .or_else(|| (candidates.len() == 1).then(|| candidates[0]));
        match chosen {
            Some(found) => {
                let subst = overload::infer_type_arguments(self.db(), &found.method, args);
                found.method.ret.substitute(&subst)
            }
            None => Type::Unknown,
        }
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

/// The declared type an argument at `index` is passed as: for the spread tail
/// of a varargs call, the element type.
fn param_for_argument(method: &MethodData, index: usize, spread: bool) -> Option<Type> {
    let params = &method.params;
    if spread && index + 1 >= params.len() {
        return match &params.last()?.ty {
            Type::Array(element) => Some((**element).clone()),
            _ => None,
        };
    }
    params.get(index).map(|param| param.ty.clone())
}

//! What is in scope at a position in a file.
//!
//! Everything the IDE layer does starts here: completion, hover, definition,
//! and diagnostics all need the same answer to "what does this name mean, at
//! this offset, inside this method, in this class".

use crate::db::Db;
use crate::lower;
use crate::name::{self, Name};
use crate::outline::FileOutline;
use crate::resolve::Scope;
use crate::symbols::{ClassRef, FieldData, FileId};
use crate::types::{Type, TypeVar};
use java_syntax::{ast, Node, SourceFile};
use std::sync::Arc;

/// A local variable, parameter, or loop or catch variable.
#[derive(Debug, Clone)]
pub struct Local {
    pub name: Name,
    pub ty: Type,
    /// Byte range of the declared name, for go-to-definition.
    pub name_range: (u32, u32),
    pub is_parameter: bool,
}

/// What a simple name at some position denotes.
#[derive(Debug, Clone)]
pub enum Resolved {
    Local(Local),
    Field {
        owner: ClassRef,
        field: FieldData,
        /// False for a static field referenced without a receiver.
        through_instance: bool,
    },
    Class(Name),
    TypeVar(TypeVar),
    Package(String),
}

impl Resolved {
    /// The type of the expression this name forms, where it has one.
    pub fn value_type(&self) -> Option<Type> {
        match self {
            Resolved::Local(local) => Some(local.ty.clone()),
            Resolved::Field { field, .. } => Some(field.ty.clone()),
            _ => None,
        }
    }
}

/// The resolution context at one position.
///
/// Cloning is cheap: everything sizeable behind it is reference counted, which
/// lets a caller keep a context for reuse across the nodes of a statement.
#[derive(Clone)]
pub struct Context<'a> {
    pub db: &'a Db,
    pub file: FileId,
    pub source: Arc<SourceFile>,
    pub outline: Arc<FileOutline>,
    /// Innermost enclosing class, when the position is inside one.
    pub class: Option<ClassRef>,
    /// Enclosing classes, innermost first.
    pub enclosing: Vec<Name>,
    /// Type of `this`, absent in a static context.
    pub self_type: Option<Type>,
    pub in_static_context: bool,
    /// Locals visible at the position, innermost declaration first.
    pub locals: Vec<Local>,
    /// Type parameters in scope, from the class and the enclosing method.
    pub type_vars: Vec<(Name, Vec<Type>)>,
    /// The enclosing method or constructor declaration, if any.
    pub method_node_range: Option<(u32, u32)>,
}

impl<'a> Context<'a> {
    pub fn text(&self) -> &str {
        self.source.text()
    }

    pub fn package(&self) -> &str {
        self.outline.package.as_deref().unwrap_or("")
    }

    /// A type-name resolver sharing this context's scope.
    pub fn type_scope(&'a self) -> Scope<'a> {
        Scope {
            db: self.db,
            outline: &self.outline,
            enclosing: self.enclosing.clone(),
            type_vars: self.type_vars.clone(),
        }
    }

    pub fn local(&self, name: &str) -> Option<&Local> {
        self.locals.iter().find(|l| &*l.name == name)
    }

    /// Resolves a simple name the way the language does: locals shadow fields,
    /// fields shadow types, and a type shadows a package (JLS 6.5.2).
    pub fn resolve_name(&self, name: &str) -> Option<Resolved> {
        if let Some(local) = self.local(name) {
            return Some(Resolved::Local(local.clone()));
        }
        if let Some(found) = self.resolve_field(name) {
            return Some(found);
        }
        if let Some((var, bounds)) = self.type_vars.iter().rev().find(|(n, _)| &**n == name) {
            return Some(Resolved::TypeVar(TypeVar::new(
                var.clone(),
                self.enclosing.first().cloned(),
                bounds.clone(),
            )));
        }
        if let Some(binary) = self.type_scope().resolve_class_name(name) {
            return Some(Resolved::Class(binary));
        }
        // A static import can bring in a field by simple name.
        if let Some(found) = self.resolve_static_import_field(name) {
            return Some(found);
        }
        if self.db.is_package(name) {
            return Some(Resolved::Package(name.to_string()));
        }
        None
    }

    /// Looks for a field on the enclosing classes and what they inherit.
    pub fn resolve_field(&self, name: &str) -> Option<Resolved> {
        for (depth, enclosing) in self.enclosing.iter().enumerate() {
            let class = self.db.class(enclosing)?;
            let self_type = lower::self_type_of(&class);
            let members = crate::members::members_of(
                self.db,
                &self_type,
                crate::members::LookupOptions {
                    include_private: true,
                    // Outside its own instance, only statics of an enclosing
                    // class are reachable.
                    include_instance: !self.in_static_context || depth > 0,
                    ..crate::members::LookupOptions::default()
                },
            );
            if let Some(found) = members.field(name) {
                if self.in_static_context && !found.field.modifiers.is_static && depth == 0 {
                    // Reachable name, but not from a static context; report it
                    // so diagnostics can explain rather than claim it is absent.
                    return Some(Resolved::Field {
                        owner: found.owner.clone(),
                        field: found.field.clone(),
                        through_instance: true,
                    });
                }
                return Some(Resolved::Field {
                    owner: found.owner.clone(),
                    field: found.field.clone(),
                    through_instance: !found.field.modifiers.is_static,
                });
            }
        }
        None
    }

    fn resolve_static_import_field(&self, name: &str) -> Option<Resolved> {
        for import in &self.outline.imports {
            if !import.is_static {
                continue;
            }
            let (owner, member) = if import.is_wildcard {
                (import.path.to_string(), name)
            } else {
                if import.simple_name() != name {
                    continue;
                }
                let dot = import.path.rfind('.')?;
                (import.path[..dot].to_string(), &import.path[dot + 1..])
            };
            let scope = self.type_scope();
            let Some(class) = scope.resolve_class(&owner) else {
                continue;
            };
            let members = crate::members::members_of(
                self.db,
                &Type::class(class.name.clone()),
                crate::members::LookupOptions::statics(),
            );
            if let Some(found) = members.field(member) {
                return Some(Resolved::Field {
                    owner: found.owner.clone(),
                    field: found.field.clone(),
                    through_instance: false,
                });
            }
        }
        None
    }
}

/// Builds the resolution context for a position in a file.
pub fn context_at<'a>(db: &'a Db, file: FileId, offset: u32) -> Option<Context<'a>> {
    let source = db.source(file)?;
    // Which class encloses the cursor is a resolution question, so it is
    // answered from the retained parse: a momentary recovery failure must not
    // make the surrounding class disappear while the user is typing in it.
    let (_, resolution_outline) = db.resolution_source(file)?;
    let outline = db.outline(file)?;
    let text = source.text();

    // Enclosing classes, innermost first, following the outline's parent links.
    let mut enclosing = Vec::new();
    let mut index = resolution_outline
        .class_at(offset)
        .and_then(|innermost| {
            resolution_outline
                .classes
                .iter()
                .position(|c| c.binary == innermost.binary)
        })
        .map(|i| i as u32);
    while let Some(at) = index {
        let class = &resolution_outline.classes[at as usize];
        enclosing.push(class.binary.clone());
        index = class.parent;
    }

    let class = enclosing.first().and_then(|name| db.class(name));
    let mut type_vars: Vec<(Name, Vec<Type>)> = Vec::new();
    // Outer classes first so inner declarations shadow them.
    for name in enclosing.iter().rev() {
        if let Some(outer) = db.class(name) {
            for param in &outer.type_params {
                type_vars.push((param.name.clone(), param.bounds.clone()));
            }
        }
    }

    let node = ast::node_at(source.tree(), offset);
    // A method's type parameters must be known before its parameter and local
    // types are lowered, or `T item` would resolve to nothing.
    type_vars.extend(method_type_parameters(
        node,
        text,
        db,
        &resolution_outline,
        &enclosing,
        &type_vars,
    ));
    let scope_for_types = Scope {
        db,
        outline: &resolution_outline,
        enclosing: enclosing.clone(),
        type_vars: type_vars.clone(),
    };

    let mut locals = Vec::new();
    let mut in_static_context = false;
    let mut method_node_range = None;

    for ancestor in ast::ancestors(node) {
        match ancestor.kind() {
            "block" | "constructor_body" | "switch_block" | "switch_block_statement_group" => {
                collect_block_locals(ancestor, offset, text, &scope_for_types, &mut locals);
                // `if (!(o instanceof T t)) return;` leaves `t` in scope for the rest of the block.
                for statement in
                    ast::named_children(ancestor).filter(|s| s.start_byte() as u32 <= offset)
                {
                    collect_pattern_bindings(statement, text, &scope_for_types, &mut locals);
                }
                if matches!(ancestor.kind(), "switch_block_statement_group") {
                    collect_case_bindings(ancestor, text, &scope_for_types, &mut locals);
                }
            }
            "switch_rule" => {
                collect_case_bindings(ancestor, text, &scope_for_types, &mut locals);
            }
            "if_statement"
            | "while_statement"
            | "do_statement"
            | "conditional_expression"
            | "binary_expression" => {
                collect_pattern_bindings(ancestor, text, &scope_for_types, &mut locals);
            }
            "for_statement" => {
                if let Some(init) = ast::field(ancestor, "init") {
                    collect_declaration(init, text, &scope_for_types, &mut locals);
                }
                for child in ast::named_children(ancestor) {
                    if child.kind() == "local_variable_declaration" {
                        collect_declaration(child, text, &scope_for_types, &mut locals);
                    }
                }
            }
            "enhanced_for_statement" => {
                if let (Some(name_node), Some(type_node)) =
                    (ast::field(ancestor, "name"), ast::field(ancestor, "type"))
                {
                    locals.push(Local {
                        name: name::name(ast::text(name_node, text)),
                        ty: scope_for_types.lower_type(type_node, text),
                        name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
                        is_parameter: false,
                    });
                }
            }
            "catch_clause" => {
                for parameter in
                    ast::named_children(ancestor).filter(|c| c.kind() == "catch_formal_parameter")
                {
                    let ty = ast::named_children(parameter)
                        .find(|c| c.kind() == "catch_type")
                        .and_then(|list| ast::named_children(list).next())
                        .map(|t| scope_for_types.lower_type(t, text))
                        .unwrap_or(Type::Unknown);
                    if let Some(name_node) = ast::field(parameter, "name") {
                        locals.push(Local {
                            name: name::name(ast::text(name_node, text)),
                            ty,
                            name_range: (
                                name_node.start_byte() as u32,
                                name_node.end_byte() as u32,
                            ),
                            is_parameter: true,
                        });
                    }
                }
            }
            "try_with_resources_statement" | "resource_specification" => {
                // The resources sit in a `resource_specification` child of the statement.
                let specification = if ancestor.kind() == "resource_specification" {
                    Some(ancestor)
                } else {
                    ast::field(ancestor, "resources")
                };
                let resources = specification
                    .into_iter()
                    .flat_map(ast::named_children)
                    .filter(|c| c.kind() == "resource");
                for resource in resources {
                    if let (Some(name_node), Some(type_node)) =
                        (ast::field(resource, "name"), ast::field(resource, "type"))
                    {
                        locals.push(Local {
                            name: name::name(ast::text(name_node, text)),
                            ty: resource_type(type_node, text, &scope_for_types),
                            name_range: (
                                name_node.start_byte() as u32,
                                name_node.end_byte() as u32,
                            ),
                            is_parameter: false,
                        });
                    }
                }
            }
            "lambda_expression" => {
                collect_lambda_params(ancestor, text, &scope_for_types, &mut locals);
            }
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => {
                method_node_range =
                    Some((ancestor.start_byte() as u32, ancestor.end_byte() as u32));
                in_static_context =
                    in_static_context || crate::outline::has_modifier(ancestor, text, "static");
                collect_parameters(ancestor, text, &scope_for_types, &mut locals);
                // A compact constructor has no parameter list; the record's components are its
                // parameters.
                if ancestor.kind() == "compact_constructor_declaration" {
                    if let Some(record) =
                        ast::ancestors(ancestor).find(|a| a.kind() == "record_declaration")
                    {
                        collect_parameters(record, text, &scope_for_types, &mut locals);
                    }
                }
            }
            "field_declaration" => {
                in_static_context =
                    in_static_context || crate::outline::has_modifier(ancestor, text, "static");
            }
            "static_initializer" => in_static_context = true,
            _ => {}
        }
    }
    let self_type = class
        .as_ref()
        .filter(|_| !in_static_context)
        .map(|c| lower::self_type_of(c));

    let mut context = Context {
        db,
        file,
        source,
        outline,
        class,
        enclosing,
        self_type,
        in_static_context,
        locals,
        type_vars,
        method_node_range,
    };
    // A lambda written without parameter types takes them from the functional
    // interface it is being assigned to. That needs inference, which needs a
    // context, so it happens in a second pass over the one just built.
    resolve_lambda_parameter_types(&mut context, offset);
    Some(context)
}

/// The type parameters of every method or constructor enclosing `node`,
/// outermost first, with their bounds.
///
/// Two passes: the names are registered first, without bounds, so that a bound
/// may mention any variable including its own (`T extends Comparable<? super T>`);
/// then each bound is lowered against that scope.
fn method_type_parameters(
    node: Node<'_>,
    text: &str,
    db: &Db,
    outline: &FileOutline,
    enclosing: &[Name],
    outer: &[(Name, Vec<Type>)],
) -> Vec<(Name, Vec<Type>)> {
    let mut declarations: Vec<(Name, Vec<Node<'_>>)> = Vec::new();
    let methods: Vec<Node<'_>> = ast::ancestors(node)
        .filter(|a| matches!(a.kind(), "method_declaration" | "constructor_declaration"))
        .collect();
    for method in methods.into_iter().rev() {
        let Some(list) = ast::field(method, "type_parameters") else {
            continue;
        };
        for declaration in ast::named_children(list).filter(|c| c.kind() == "type_parameter") {
            let Some(name_node) = ast::named_children(declaration)
                .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
            else {
                continue;
            };
            let bounds = ast::named_children(declaration)
                .find(|c| c.kind() == "type_bound")
                .map(|bound| ast::named_children(bound).collect())
                .unwrap_or_default();
            declarations.push((name::name(ast::text(name_node, text)), bounds));
        }
    }
    if declarations.is_empty() {
        return Vec::new();
    }
    let mut known: Vec<(Name, Vec<Type>)> = outer.to_vec();
    known.extend(
        declarations
            .iter()
            .map(|(name, _)| (name.clone(), Vec::new())),
    );
    let scope = Scope {
        db,
        outline,
        enclosing: enclosing.to_vec(),
        type_vars: known,
    };
    declarations
        .into_iter()
        .map(|(name, bounds)| {
            let lowered = bounds
                .into_iter()
                .map(|b| scope.lower_type(b, text))
                .collect();
            (name, lowered)
        })
        .collect()
}

/// Gives untyped lambda parameters the types their target interface declares.
///
/// `(a, b) -> a.compareTo(b)` assigned to a `Comparator<String>` makes `a` and
/// `b` strings. Without this they stay unknown, and nothing can be offered on
/// them — which is correct but unhelpful, and this is the common way to write
/// a comparator.
fn resolve_lambda_parameter_types(context: &mut Context<'_>, offset: u32) {
    if !context.locals.iter().any(|local| local.ty.is_unknown()) {
        return;
    }
    let source = context.source.clone();
    let node = ast::node_at(source.tree(), offset);
    let lambdas: Vec<Node<'_>> = ast::ancestors(node)
        .filter(|ancestor| ancestor.kind() == "lambda_expression")
        .collect();
    for lambda in lambdas {
        let names = lambda_parameter_names(lambda, context.text());
        if names.is_empty() {
            continue;
        }
        if !names.iter().any(|name| {
            context
                .local(name)
                .is_some_and(|local| local.ty.is_unknown())
        }) {
            continue;
        }
        let inferer = crate::infer::Inferer::new(context);
        let Some(target) = inferer.target_type(lambda) else {
            continue;
        };
        let Some(method) = crate::members::functional_interface_method(context.db, &target) else {
            continue;
        };
        if method.method.params.len() != names.len() {
            continue;
        }
        let resolved: Vec<Type> = method
            .method
            .params
            .iter()
            .map(|param| lambda_parameter_type(&param.ty))
            .collect();
        for (name, ty) in names.iter().zip(resolved) {
            if let Some(local) = context
                .locals
                .iter_mut()
                .find(|local| &*local.name == name.as_str() && local.ty.is_unknown())
            {
                local.ty = ty;
            }
        }
    }
}

/// A wildcard in a functional interface's parameter position stands for the
/// type the lambda will actually receive, so its bound is used directly:
/// `Consumer<? super String>.accept(? super String)` yields `String`.
pub(crate) fn lambda_parameter_type(declared: &Type) -> Type {
    match declared {
        Type::Wildcard {
            bound: Some(bound), ..
        } => lambda_parameter_type(bound),
        Type::Wildcard { bound: None, .. } => Type::object(),
        other => other.clone(),
    }
}

/// Names declared by a lambda's parameter list, in order.
pub(crate) fn lambda_parameter_names(lambda: Node<'_>, text: &str) -> Vec<String> {
    let Some(parameters) = ast::field(lambda, "parameters") else {
        return Vec::new();
    };
    match parameters.kind() {
        "identifier" => vec![ast::text(parameters, text).to_string()],
        "inferred_parameters" => ast::named_children(parameters)
            .filter(|c| c.kind() == "identifier")
            .map(|c| ast::text(c, text).to_string())
            .collect(),
        "formal_parameters" => ast::named_children(parameters)
            .filter_map(|parameter| {
                Some(ast::text(ast::field(parameter, "name")?, text).to_string())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Collects declarations in a block that precede `offset`; a local is not in
/// scope before its own declaration.
pub(crate) fn collect_block_locals(
    block: Node<'_>,
    offset: u32,
    text: &str,
    scope: &Scope<'_>,
    out: &mut Vec<Local>,
) {
    for statement in ast::named_children(block) {
        if statement.start_byte() as u32 > offset {
            break;
        }
        if statement.kind() == "local_variable_declaration" {
            collect_declaration(statement, text, scope, out);
        }
    }
}

/// The declared type of a try-with-resources variable; `var` is left unknown rather than guessed.
fn resource_type(type_node: Node<'_>, text: &str, scope: &Scope<'_>) -> Type {
    if type_node.kind() == "type_identifier" && ast::text(type_node, text) == "var" {
        Type::Unknown
    } else {
        scope.lower_type(type_node, text)
    }
}

/// Adds the variables introduced by patterns in `node`'s expressions.
///
/// Flow scoping (where a binding is definitely assigned) is not modelled: a binding is visible from
/// its pattern to the end of the enclosing statement or block. That can accept a use Java rejects,
/// but never rejects a valid one. Nested blocks are skipped so a binding inside one `if` body does
/// not leak into its siblings.
fn collect_pattern_bindings(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    if matches!(node.kind(), "block" | "class_body" | "switch_block") {
        return;
    }
    match node.kind() {
        "instanceof_expression" => {
            if let (Some(ty), Some(name_node)) =
                (ast::field(node, "right"), ast::field(node, "name"))
            {
                out.push(named_binding(name_node, text, scope.lower_type(ty, text)));
            }
            if let Some(pattern) = ast::field(node, "pattern") {
                bind_pattern(pattern, text, scope, out);
            }
        }
        "type_pattern" | "record_pattern" => bind_pattern(node, text, scope, out),
        _ => {}
    }
    for child in ast::named_children(node) {
        collect_pattern_bindings(child, text, scope, out);
    }
}

/// Bindings from a `case` label's patterns, visible in its guard and body.
fn collect_case_bindings(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    for label in ast::named_children(node).filter(|c| c.kind() == "switch_label") {
        let mut cursor = label.walk();
        for child in label.named_children(&mut cursor) {
            bind_pattern(child, text, scope, out);
        }
    }
}

/// Declares the variables of one pattern: `Type name`, or the components of `Record(...)`.
fn bind_pattern(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    match node.kind() {
        "pattern" | "guard" => {
            for child in ast::named_children(node) {
                bind_pattern(child, text, scope, out);
            }
        }
        "type_pattern" => {
            let parts: Vec<Node<'_>> = ast::named_children(node)
                .filter(|c| c.kind() != "modifiers")
                .collect();
            if let [ty, name_node] = parts.as_slice() {
                if name_node.kind() == "identifier" {
                    out.push(named_binding(*name_node, text, scope.lower_type(*ty, text)));
                }
            }
        }
        "record_pattern" => {
            let record = ast::named_children(node)
                .find(|c| c.kind() != "record_pattern_body")
                .map(|ty| scope.lower_type(ty, text))
                .unwrap_or(Type::Unknown);
            let component_types = record_component_types(scope, &record);
            let Some(body) = ast::named_children(node).find(|c| c.kind() == "record_pattern_body")
            else {
                return;
            };
            for (index, component) in ast::named_children(body).enumerate() {
                match component.kind() {
                    "record_pattern_component" => {
                        let parts: Vec<Node<'_>> = ast::named_children(component)
                            .filter(|c| c.kind() != "modifiers")
                            .collect();
                        match parts.as_slice() {
                            [ty, name_node] if name_node.kind() == "identifier" => {
                                let declared = if ty.kind() == "type_identifier"
                                    && ast::text(*ty, text) == "var"
                                {
                                    component_types.get(index).cloned().unwrap_or(Type::Unknown)
                                } else {
                                    scope.lower_type(*ty, text)
                                };
                                out.push(named_binding(*name_node, text, declared));
                            }
                            [nested] => bind_pattern(*nested, text, scope, out),
                            _ => {}
                        }
                    }
                    "record_pattern" => bind_pattern(component, text, scope, out),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn named_binding(name_node: Node<'_>, text: &str, ty: Type) -> Local {
    Local {
        name: name::name(ast::text(name_node, text)),
        ty,
        name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
        is_parameter: false,
    }
}

/// The declared component types of a record, in order, with any that mention a type variable
/// replaced by `Unknown` rather than guessed.
fn record_component_types(scope: &Scope<'_>, record: &Type) -> Vec<Type> {
    let Some(class) = record.class_name().and_then(|n| scope.db.class(n)) else {
        return Vec::new();
    };
    class
        .fields
        .iter()
        .filter(|f| !f.modifiers.is_static)
        .map(|f| {
            if matches!(f.ty, Type::Var(_))
                || class
                    .type_params
                    .iter()
                    .any(|p| f.ty.display_full().contains(&*p.name))
            {
                Type::Unknown
            } else {
                f.ty.clone()
            }
        })
        .collect()
}

fn collect_declaration(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    if node.kind() != "local_variable_declaration" {
        return;
    }
    let declared = ast::field(node, "type")
        .map(|t| scope.lower_type(t, text))
        .unwrap_or(Type::Unknown);
    for declarator in ast::named_children(node).filter(|c| c.kind() == "variable_declarator") {
        let Some(name_node) = ast::field(declarator, "name") else {
            continue;
        };
        let extra = ast::named_children(declarator)
            .filter(|c| c.kind() == "dimensions")
            .map(|d| ast::text(d, text).matches('[').count())
            .sum::<usize>();
        let ty = (0..extra).fold(declared.clone(), |inner, _| Type::Array(Box::new(inner)));
        out.push(Local {
            name: name::name(ast::text(name_node, text)),
            ty,
            name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
            is_parameter: false,
        });
    }
}

fn collect_parameters(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    let Some(list) = ast::field(node, "parameters") else {
        return;
    };
    for parameter in ast::named_children(list) {
        match parameter.kind() {
            "formal_parameter" => {
                let ty = ast::field(parameter, "type")
                    .map(|t| scope.lower_type(t, text))
                    .unwrap_or(Type::Unknown);
                if let Some(name_node) = ast::field(parameter, "name") {
                    out.push(Local {
                        name: name::name(ast::text(name_node, text)),
                        ty,
                        name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
                        is_parameter: true,
                    });
                }
            }
            "spread_parameter" => {
                let element = ast::named_children(parameter)
                    .find(|c| !matches!(c.kind(), "variable_declarator" | "modifiers"))
                    .map(|t| scope.lower_type(t, text))
                    .unwrap_or(Type::Unknown);
                if let Some(name_node) = ast::named_children(parameter)
                    .find(|c| c.kind() == "variable_declarator")
                    .and_then(|d| ast::field(d, "name"))
                {
                    out.push(Local {
                        name: name::name(ast::text(name_node, text)),
                        ty: Type::Array(Box::new(element)),
                        name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
                        is_parameter: true,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Lambda parameters. A typed parameter list gives its own types; an inferred
/// list does not, and the target type is applied later by the inferencer.
fn collect_lambda_params(node: Node<'_>, text: &str, scope: &Scope<'_>, out: &mut Vec<Local>) {
    let Some(parameters) = ast::field(node, "parameters") else {
        return;
    };
    match parameters.kind() {
        "formal_parameters" => collect_parameters(node, text, scope, out),
        "inferred_parameters" => {
            for name_node in ast::named_children(parameters).filter(|c| c.kind() == "identifier") {
                out.push(Local {
                    name: name::name(ast::text(name_node, text)),
                    ty: Type::Unknown,
                    name_range: (name_node.start_byte() as u32, name_node.end_byte() as u32),
                    is_parameter: true,
                });
            }
        }
        "identifier" => out.push(Local {
            name: name::name(ast::text(parameters, text)),
            ty: Type::Unknown,
            name_range: (parameters.start_byte() as u32, parameters.end_byte() as u32),
            is_parameter: true,
        }),
        _ => {}
    }
}

//! Diagnostics.
//!
//! Two rules govern everything here:
//!
//! * A semantic error is reported only when every type involved resolved. If
//!   anything is `Unknown`, the analysis is incomplete and staying silent is
//!   the only honest option.
//! * Nothing inside a region the parser had to recover from is checked, since
//!   the tree there does not reflect what the author wrote.
//!
//! Syntax errors are reported separately and always, because a parse failure
//! is a fact about the text rather than a judgement about its meaning.

use crate::constants;
use crate::convert;
use crate::render;
use java_semantic::infer::{Inferer, Receiver};
use java_semantic::members::LookupOptions;
use java_semantic::name;
use java_semantic::overload;
use java_semantic::scope::{self, Context};
use java_semantic::symbols::{FileId, Visibility};
use java_semantic::{Db, Type};
use java_syntax::{ast, Node};
use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString, Range};
use project_model::SourceLevel;
use rustc_hash::FxHashSet;

/// Stable identifiers for each diagnostic, used by code actions.
pub mod codes {
    pub const SYNTAX: &str = "syntax";
    pub const UNKNOWN_TYPE: &str = "unknown-type";
    pub const UNKNOWN_SYMBOL: &str = "unknown-symbol";
    pub const UNKNOWN_MEMBER: &str = "unknown-member";
    pub const ARGUMENT_COUNT: &str = "argument-count";
    pub const ARGUMENT_TYPE: &str = "argument-type";
    pub const INCOMPATIBLE_TYPES: &str = "incompatible-types";
    pub const DUPLICATE_DECLARATION: &str = "duplicate-declaration";
    pub const RETURN_TYPE: &str = "return-type";
    pub const INACCESSIBLE: &str = "inaccessible";
    pub const UNUSED_IMPORT: &str = "unused-import";
    pub const LANGUAGE_LEVEL: &str = "language-level";
}

/// Upper bound on diagnostics per file. A file in an unusual state can produce
/// a great many; the editor cannot usefully show them all and the client
/// should not have to transfer them.
const MAX_DIAGNOSTICS: usize = 200;

pub fn diagnostics(db: &Db, file: FileId) -> Vec<Diagnostic> {
    let Some(source) = db.source(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();

    for error in source.syntax_errors() {
        if out.len() >= MAX_DIAGNOSTICS {
            return out;
        }
        if let Some(range) = convert::range_of(db, file, (error.start, error.end)) {
            out.push(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String(codes::SYNTAX.to_string())),
                source: Some("ferrisjdt".to_string()),
                message: format!("syntax error: {}", error.message),
                ..Default::default()
            });
        }
    }

    let Some(context) = scope::context_at(db, file, 0) else {
        return out;
    };
    let mut checker = Checker {
        db,
        file,
        context: &context,
        level: db.project().source_level,
        out,
        reported: Vec::new(),
        cached_context: None,
    };
    checker.check_unused_imports();
    checker.walk(source.tree().root_node());
    checker.out
}

struct Checker<'a> {
    db: &'a Db,
    file: FileId,
    context: &'a Context<'a>,
    level: SourceLevel,
    out: Vec<Diagnostic>,
    /// Byte ranges already reported, used to suppress cascading errors.
    reported: Vec<(u32, u32)>,
    /// The most recently built context, keyed by the statement it belongs to.
    ///
    /// Which names are in scope changes only at statement boundaries, so one
    /// context serves every node in a statement. Rebuilding it per node was
    /// the dominant cost of checking a large file.
    cached_context: Option<(u32, Context<'a>)>,
}

impl<'a> Checker<'a> {
    fn push(
        &mut self,
        range: (u32, u32),
        severity: DiagnosticSeverity,
        code: &str,
        message: String,
    ) {
        if self.out.len() >= MAX_DIAGNOSTICS {
            return;
        }
        let Some(range) = convert::range_of(self.db, self.file, range) else {
            return;
        };
        self.out.push(Diagnostic {
            range,
            severity: Some(severity),
            code: Some(NumberOrString::String(code.to_string())),
            source: Some("ferrisjdt".to_string()),
            message,
            ..Default::default()
        });
    }

    fn error(&mut self, node: Node<'_>, code: &str, message: String) {
        self.push(
            (node.start_byte() as u32, node.end_byte() as u32),
            DiagnosticSeverity::ERROR,
            code,
            message,
        );
        self.reported
            .push((node.start_byte() as u32, node.end_byte() as u32));
    }

    /// Whether something inside this range was already reported. A second
    /// diagnostic about an expression that is already known to be wrong is
    /// noise, which is why `javac` reports only the innermost cause.
    fn already_reported_within(&self, range: (u32, u32)) -> bool {
        self.reported
            .iter()
            .any(|(start, end)| *start >= range.0 && *end <= range.1)
    }

    fn text(&self) -> &str {
        self.context.text()
    }

    /// Walks the tree, skipping any subtree the parser recovered inside.
    fn walk(&mut self, node: Node<'_>) {
        if self.out.len() >= MAX_DIAGNOSTICS {
            return;
        }
        if node.is_error() || node.is_missing() {
            return;
        }
        // A statement containing a parse error is not analysed: the tree there
        // is a guess, and checking it would produce invented diagnostics.
        if is_statement(node.kind()) && node.has_error() {
            return;
        }
        self.check(node);
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            self.walk(child);
        }
        // Assignability is checked after the subtree so that an error inside
        // the value suppresses the cascading error about the assignment.
        match node.kind() {
            "local_variable_declaration" => self.check_local_declaration(node),
            "return_statement" => self.check_return(node),
            _ => {}
        }
    }

    fn check(&mut self, node: Node<'_>) {
        match node.kind() {
            "type_identifier" | "scoped_type_identifier" => self.check_type_name(node),
            "identifier" => self.check_identifier(node),
            "field_access" => self.check_field_access(node),
            "method_invocation" => self.check_invocation(node),
            "object_creation_expression" => self.check_creation(node),
            "lambda_expression" | "method_reference" => {
                if !self.level.allows_lambdas() {
                    self.error(
                        node,
                        codes::LANGUAGE_LEVEL,
                        format!(
                            "lambdas require source level 8 (configured: {})",
                            self.level
                        ),
                    );
                }
            }
            "switch_expression" => {
                // The grammar uses this node for the classic `switch` statement too, which every
                // level accepts; only arrow rules and a switch used as a value need Java 14.
                if !self.level.allows_switch_expressions() && is_modern_switch(node) {
                    self.error(
                        node,
                        codes::LANGUAGE_LEVEL,
                        format!(
                            "switch expressions require source level 14 (configured: {})",
                            self.level
                        ),
                    );
                }
            }
            "record_declaration" => {
                if !self.level.allows_records() {
                    self.error(
                        node,
                        codes::LANGUAGE_LEVEL,
                        format!(
                            "records require source level 16 (configured: {})",
                            self.level
                        ),
                    );
                }
            }
            "text_block" => {
                if !self.level.allows_text_blocks() {
                    self.error(
                        node,
                        codes::LANGUAGE_LEVEL,
                        format!(
                            "text blocks require source level 15 (configured: {})",
                            self.level
                        ),
                    );
                }
            }
            "block" | "constructor_body" => self.check_duplicate_locals(node),
            "method_declaration" => self.check_missing_return(node),
            _ => {}
        }
    }

    /// A type name that does not resolve.
    fn check_type_name(&mut self, node: Node<'_>) {
        // Only check the outermost name of a qualified type, and skip the
        // segments of a scoped name, which are handled as a whole.
        if let Some(parent) = node.parent() {
            if matches!(
                parent.kind(),
                "scoped_type_identifier" | "scoped_identifier"
            ) {
                return;
            }
            // The `name` of a declaration is being defined, not referenced.
            if ast::field(parent, "name").map(|n| n.id()) == Some(node.id()) {
                return;
            }
            if parent.kind() == "type_parameter" {
                return;
            }
            // `outer.new Inner()`: `Inner` is looked up in the outer instance's class.
            if parent.kind() == "object_creation_expression"
                && ast::creation_qualifier(parent).is_some()
            {
                return;
            }
        }
        let written = java_semantic::outline::dotted(node, self.text());
        // The grammar reads the `Outer.super` of `Outer.super::add` as a scoped type name.
        if written.ends_with(".super") {
            return;
        }
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        if scope.type_vars.iter().any(|(n, _)| **n == *written) {
            return;
        }
        if scope.type_scope().resolve_class_name(&written).is_some() {
            return;
        }
        // `var` at a source level that allows it is not a type name.
        if written == "var" && self.level.allows_var() {
            return;
        }
        let simple = name::simple_name(&written);
        let importable = self.db.classes_named(simple);
        let hint = match importable.first() {
            Some(candidate) if !written.contains('.') => {
                format!("; did you mean to import {}?", name::source_name(candidate))
            }
            _ => String::new(),
        };
        self.error(
            node,
            codes::UNKNOWN_TYPE,
            format!("cannot find type `{written}`{hint}"),
        );
    }

    /// An identifier used as a value that resolves to nothing.
    fn check_identifier(&mut self, node: Node<'_>) {
        let Some(parent) = node.parent() else { return };
        // Only simple names in an expression position are value references.
        let is_value_use = match parent.kind() {
            "expression_statement"
            | "argument_list"
            | "binary_expression"
            | "unary_expression"
            | "parenthesized_expression"
            | "return_statement"
            | "array_access"
            | "update_expression"
            | "ternary_expression"
            | "instanceof_expression" => true,
            "assignment_expression" => true,
            "variable_declarator" => ast::field(parent, "value").map(|n| n.id()) == Some(node.id()),
            "field_access" => ast::field(parent, "object").map(|n| n.id()) == Some(node.id()),
            "method_invocation" => ast::field(parent, "object").map(|n| n.id()) == Some(node.id()),
            "enhanced_for_statement" => {
                ast::field(parent, "value").map(|n| n.id()) == Some(node.id())
            }
            _ => false,
        };
        if !is_value_use {
            return;
        }
        let written = ast::text(node, self.text()).to_string();
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        if scope.resolve_name(&written).is_some() {
            return;
        }
        // A label or a name inside an annotation is not a variable.
        if ast::ancestor_of_kind(
            node,
            &["annotation", "marker_annotation", "labeled_statement"],
        )
        .is_some()
        {
            return;
        }
        self.error(
            node,
            codes::UNKNOWN_SYMBOL,
            format!("cannot find symbol `{written}`"),
        );
    }

    fn check_field_access(&mut self, node: Node<'_>) {
        let Some(field_node) = ast::field(node, "field") else {
            return;
        };
        let Some(object) = ast::field(node, "object") else {
            return;
        };
        let written = ast::text(field_node, self.text()).to_string();
        let written = written.as_str();
        if matches!(written, "class" | "this" | "length") {
            return;
        }
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let written = written.to_string();
        let written = written.as_str();
        let inferer = Inferer::new(&scope);
        let Some(receiver) = inferer.receiver_of(object, 0) else {
            return;
        };
        match receiver {
            Receiver::Value(ty) | Receiver::Super(ty) => {
                if ty.has_unknown() {
                    return;
                }
                let members = inferer.members_for(&ty, LookupOptions::instance());
                if members.field(written).is_some() {
                    return;
                }
                // The name may be a method reference or a nested type rather
                // than a field; only complain when nothing at all matches.
                if !members.methods_named(written).is_empty() {
                    return;
                }
                if members
                    .nested
                    .iter()
                    .any(|n| name::simple_name(n) == written)
                {
                    return;
                }
                // The member may exist and simply not be visible from here,
                // which is a different mistake and deserves a different
                // message.
                if let Some(owner) = self.declaring_class_of_hidden_field(&ty, written) {
                    self.error(
                        field_node,
                        codes::INACCESSIBLE,
                        format!(
                            "`{written}` is not accessible in `{}`",
                            name::source_name(&owner)
                        ),
                    );
                    return;
                }
                self.error(
                    field_node,
                    codes::UNKNOWN_MEMBER,
                    format!("`{}` has no member `{written}`", ty.display_short()),
                );
            }
            Receiver::StaticClass(binary) => {
                let ty = Type::class(binary.clone());
                let members = inferer.members_for(&ty, LookupOptions::default());
                if members.field(written).is_some()
                    || !members.methods_named(written).is_empty()
                    || java_semantic::resolve::member_type(self.db, &binary, written).is_some()
                {
                    return;
                }
                self.error(
                    field_node,
                    codes::UNKNOWN_MEMBER,
                    format!("`{}` has no member `{written}`", name::source_name(&binary)),
                );
            }
            Receiver::Package(_) => {}
        }
    }

    fn check_invocation(&mut self, node: Node<'_>) {
        let Some(name_node) = ast::field(node, "name") else {
            return;
        };
        let written = ast::text(name_node, self.text()).to_string();
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let inferer = Inferer::new(&scope);

        // The receiver has to be known before anything can be said.
        if let Some(object) = ast::field(node, "object") {
            match inferer.receiver_of(object, 0) {
                Some(Receiver::Value(ty)) | Some(Receiver::Super(ty)) if ty.has_unknown() => return,
                Some(Receiver::Package(_)) | None => return,
                _ => {}
            }
        }
        let Some((_, candidates)) = inferer.call_candidates(node) else {
            return;
        };
        if candidates.is_empty() {
            // A functional-interface call on a lambda-typed value, or a method
            // this layer cannot see, must not be reported as missing.
            let receiver_known = match ast::field(node, "object") {
                Some(object) => inferer
                    .receiver_of(object, 0)
                    .and_then(|r| r.value_type().cloned())
                    .map(|ty| !ty.has_unknown())
                    .unwrap_or(false),
                None => true,
            };
            if receiver_known {
                self.error(
                    name_node,
                    codes::UNKNOWN_MEMBER,
                    format!("cannot find method `{written}`"),
                );
            }
            return;
        }

        let args: Vec<Type> = ast::field(node, "arguments")
            .map(|list| {
                ast::named_children(list)
                    .map(|argument| inferer.type_of(argument))
                    .collect()
            })
            .unwrap_or_default();

        let arity_matches: Vec<_> = candidates
            .iter()
            .filter(|found| found.method.accepts_arity(args.len()))
            .collect();
        let arity_matches = inferer.narrow_by_lambda_arity(node, arity_matches);
        if arity_matches.is_empty() {
            let expected: Vec<String> = candidates
                .iter()
                .map(|found| found.method.params.len().to_string())
                .collect();
            self.error(
                node,
                codes::ARGUMENT_COUNT,
                format!(
                    "`{written}` takes {} argument(s), but {} were given",
                    expected.join(" or "),
                    args.len()
                ),
            );
            return;
        }
        if args.iter().any(|arg| arg.has_unknown()) {
            return;
        }
        let by_reference: Vec<&java_semantic::members::FoundMethod> =
            arity_matches.iter().map(|found| &**found).collect();
        if overload::select(self.db, &by_reference, &args).is_some() {
            let enclosing = scope.enclosing.clone();
            self.check_accessibility(node, &by_reference, &args, &enclosing);
            return;
        }
        // Exactly one candidate makes it possible to say which argument is
        // wrong; with several, only that none applied can be stated.
        if arity_matches.len() == 1 {
            let method = &arity_matches[0].method;
            // A generic method is judged by its parameters as this call
            // instantiates them, not by the bare `T` it was declared with.
            let spread = java_semantic::solve::spreads_varargs(method, &args);
            let params = overload::instantiated_params(self.db, arity_matches[0], &args, spread)
                .unwrap_or_else(|| method.params.iter().map(|p| p.ty.clone()).collect());
            for (index, (argument, param_ty)) in args.iter().zip(&params).enumerate() {
                if !overload::is_assignable(self.db, argument, param_ty) {
                    let argument_node = ast::field(node, "arguments")
                        .and_then(|list| ast::named_children(list).nth(index));
                    let range = argument_node
                        .map(|n| (n.start_byte() as u32, n.end_byte() as u32))
                        .unwrap_or((node.start_byte() as u32, node.end_byte() as u32));
                    self.push(
                        range,
                        DiagnosticSeverity::ERROR,
                        codes::ARGUMENT_TYPE,
                        format!(
                            "argument {} of `{}` expects `{}` but got `{}`",
                            index + 1,
                            method.name,
                            param_ty.display_short(),
                            argument.display_short()
                        ),
                    );
                    self.reported.push(range);
                    return;
                }
            }
        }
        let rendered: Vec<String> = args.iter().map(Type::display_short).collect();
        self.error(
            node,
            codes::ARGUMENT_TYPE,
            format!(
                "no overload of `{written}` accepts ({})",
                rendered.join(", ")
            ),
        );
    }

    /// The class declaring a field of this name that exists on the receiver
    /// but is not visible from here.
    fn declaring_class_of_hidden_field(
        &self,
        ty: &Type,
        name: &str,
    ) -> Option<java_semantic::Name> {
        let all = java_semantic::members::members_of(
            self.db,
            ty,
            LookupOptions {
                include_private: true,
                include_synthetic: false,
                ..LookupOptions::default()
            },
        );
        all.field(name).map(|found| found.owner.name.clone())
    }

    /// Reports a resolved call that is not visible from here.
    fn check_accessibility(
        &mut self,
        node: Node<'_>,
        candidates: &[&java_semantic::members::FoundMethod],
        args: &[Type],
        enclosing: &[java_semantic::Name],
    ) {
        let Some(chosen) = overload::select(self.db, candidates, args) else {
            return;
        };
        if chosen.method.visibility != Visibility::Private {
            return;
        }
        // A private member is visible anywhere inside the top-level class that
        // declares it, which includes its nested and anonymous classes.
        let declaring = &chosen.owner.name;
        if enclosing
            .iter()
            .any(|e| top_level_of(e) == top_level_of(declaring))
        {
            return;
        }
        self.error(
            node,
            codes::INACCESSIBLE,
            format!(
                "`{}` is private in `{}`",
                chosen.method.name,
                name::source_name(declaring)
            ),
        );
    }

    fn check_creation(&mut self, node: Node<'_>) {
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let inferer = Inferer::new(&scope);
        let Some(type_node) = ast::field(node, "type") else {
            return;
        };
        let created = scope.type_scope().lower_type(type_node, self.text());
        if created.is_unknown() {
            // The unresolved type name is reported by check_type_name.
            return;
        }
        // An anonymous class body makes the call a subclass construction.
        if ast::named_children(node).any(|c| c.kind() == "class_body") {
            return;
        }
        let Some(class) = created.class_name().and_then(|n| self.db.class(n)) else {
            return;
        };
        if class.modifiers.is_abstract || class.is_interface() {
            self.error(
                type_node,
                codes::INCOMPATIBLE_TYPES,
                format!(
                    "`{}` is {} and cannot be instantiated",
                    created.display_short(),
                    if class.is_interface() {
                        "an interface"
                    } else {
                        "abstract"
                    }
                ),
            );
            return;
        }
        let args: Vec<Type> = ast::field(node, "arguments")
            .map(|list| {
                ast::named_children(list)
                    .map(|argument| inferer.type_of(argument))
                    .collect()
            })
            .unwrap_or_default();
        let constructors = inferer.constructors_of(&created);
        if constructors.is_empty() {
            return;
        }
        if constructors
            .iter()
            .any(|found| found.method.accepts_arity(args.len()))
        {
            return;
        }
        let expected: Vec<String> = constructors
            .iter()
            .map(|found| found.method.params.len().to_string())
            .collect();
        self.error(
            node,
            codes::ARGUMENT_COUNT,
            format!(
                "`{}` has no constructor taking {} argument(s) (expected {})",
                created.display_short(),
                args.len(),
                expected.join(" or ")
            ),
        );
    }

    fn check_local_declaration(&mut self, node: Node<'_>) {
        let Some(type_node) = ast::field(node, "type") else {
            return;
        };
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let declared = scope.type_scope().lower_type(type_node, self.text());
        if declared.has_unknown() {
            return;
        }
        let inferer = Inferer::new(&scope);
        for declarator in ast::named_children(node).filter(|c| c.kind() == "variable_declarator") {
            let Some(value) = ast::field(declarator, "value") else {
                continue;
            };
            // An array initialiser has no standalone type.
            if value.kind() == "array_initializer" {
                continue;
            }
            let actual = inferer.type_of(value);
            if actual.has_unknown() || actual.is_void() {
                continue;
            }
            let range = (value.start_byte() as u32, value.end_byte() as u32);
            if !overload::is_assignable(self.db, &actual, &declared)
                && !constants::narrows_by_constant(value, self.text(), &actual, &declared)
                && !self.already_reported_within(range)
            {
                self.error(
                    value,
                    codes::INCOMPATIBLE_TYPES,
                    format!(
                        "cannot assign `{}` to `{}`",
                        actual.display_short(),
                        declared.display_short()
                    ),
                );
            }
        }
    }

    fn check_return(&mut self, node: Node<'_>) {
        let Some(method) =
            ast::ancestor_of_kind(node, &["method_declaration", "lambda_expression"])
        else {
            return;
        };
        if method.kind() == "lambda_expression" {
            return;
        }
        let Some(declared_node) = ast::field(method, "type") else {
            return;
        };
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let declared = scope.type_scope().lower_type(declared_node, self.text());
        let value = ast::named_children(node).next();
        match (declared.is_void(), value) {
            (true, Some(value)) => {
                self.error(
                    value,
                    codes::RETURN_TYPE,
                    "cannot return a value from a void method".to_string(),
                );
            }
            (false, None) => {
                self.error(
                    node,
                    codes::RETURN_TYPE,
                    format!(
                        "this method must return a value of type `{}`",
                        declared.display_short()
                    ),
                );
            }
            (false, Some(value)) => {
                if declared.has_unknown() {
                    return;
                }
                let inferer = Inferer::new(&scope);
                let actual = inferer.type_of(value);
                if actual.has_unknown() || actual.is_void() {
                    return;
                }
                let range = (value.start_byte() as u32, value.end_byte() as u32);
                if !overload::is_assignable(self.db, &actual, &declared)
                    && !constants::narrows_by_constant(value, self.text(), &actual, &declared)
                    && !self.already_reported_within(range)
                {
                    self.error(
                        value,
                        codes::RETURN_TYPE,
                        format!(
                            "cannot return `{}` from a method declared `{}`",
                            actual.display_short(),
                            declared.display_short()
                        ),
                    );
                }
            }
            (true, None) => {}
        }
    }

    /// A non-void method whose body cannot return a value.
    ///
    /// Deliberately limited to methods containing no `return` at all: full
    /// reachability analysis is out of scope, and a partial version would
    /// report code that is in fact correct.
    fn check_missing_return(&mut self, node: Node<'_>) {
        let Some(body) = ast::field(node, "body") else {
            return;
        };
        let Some(type_node) = ast::field(node, "type") else {
            return;
        };
        let Some(scope) = self.context_for(node).cloned() else {
            return;
        };
        let declared = scope.type_scope().lower_type(type_node, self.text());
        if declared.is_void() || declared.is_unknown() {
            return;
        }
        if subtree_contains(body, &["return_statement", "throw_statement"]) {
            return;
        }
        // `while (true)` and `for (;;)` never fall through to the end.
        if subtree_contains(body, &["while_statement", "for_statement", "do_statement"]) {
            return;
        }
        let Some(name_node) = ast::field(node, "name") else {
            return;
        };
        self.error(
            name_node,
            codes::RETURN_TYPE,
            format!(
                "missing return statement: this method must return `{}`",
                declared.display_short()
            ),
        );
    }

    /// Two locals with the same name in one block.
    fn check_duplicate_locals(&mut self, block: Node<'_>) {
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        let mut duplicates = Vec::new();
        for statement in ast::named_children(block) {
            if statement.kind() != "local_variable_declaration" {
                continue;
            }
            for declarator in
                ast::named_children(statement).filter(|c| c.kind() == "variable_declarator")
            {
                let Some(name_node) = ast::field(declarator, "name") else {
                    continue;
                };
                let written = ast::text(name_node, self.text());
                if !seen.insert(written) {
                    duplicates.push((name_node, written.to_string()));
                }
            }
        }
        for (node, written) in duplicates {
            self.error(
                node,
                codes::DUPLICATE_DECLARATION,
                format!("`{written}` is already declared in this block"),
            );
        }
    }

    fn check_unused_imports(&mut self) {
        let Some(outline) = self.db.outline(self.file) else {
            return;
        };
        let Some(source) = self.db.source(self.file) else {
            return;
        };
        if source.tree().root_node().has_error() {
            // Half-written code routinely looks like it stopped using an
            // import; wait until the file parses before suggesting removal.
            return;
        }
        let text = source.text();
        let import_end = outline
            .imports
            .last()
            .map(|import| import.range.1)
            .unwrap_or(0);
        let mut used: FxHashSet<&str> = FxHashSet::default();
        let mut stack = vec![source.tree().root_node()];
        let mut cursor = source.tree().walk();
        while let Some(node) = stack.pop() {
            if node.kind() == "import_declaration" {
                continue;
            }
            if matches!(node.kind(), "identifier" | "type_identifier")
                && node.start_byte() as u32 >= import_end
            {
                used.insert(ast::text(node, text));
            }
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
        let unused: Vec<_> = outline
            .imports
            .iter()
            .filter(|import| !import.is_wildcard && !used.contains(import.simple_name()))
            .map(|import| (import.range, import.path.to_string()))
            .collect();
        for (range, path) in unused {
            if self.out.len() >= MAX_DIAGNOSTICS {
                return;
            }
            let Some(lsp_range) = convert::range_of(self.db, self.file, range) else {
                continue;
            };
            self.out.push(Diagnostic {
                range: lsp_range,
                severity: Some(DiagnosticSeverity::HINT),
                code: Some(NumberOrString::String(codes::UNUSED_IMPORT.to_string())),
                source: Some("ferrisjdt".to_string()),
                message: format!("unused import `{path}`"),
                tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                ..Default::default()
            });
        }
    }

    /// A resolution context at a specific offset, since scope depends on it.
    fn context_at(&self, offset: u32) -> Option<Context<'a>> {
        scope::context_at(self.db, self.file, offset)
    }

    /// The context for the statement containing `node`, built at most once per
    /// statement.
    fn context_for(&mut self, node: Node<'_>) -> Option<&Context<'a>> {
        let key = statement_start(node);
        if self.cached_context.as_ref().map(|(at, _)| *at) != Some(key) {
            self.cached_context = self.context_at(key).map(|context| (key, context));
        }
        self.cached_context.as_ref().map(|(_, context)| context)
    }
}

/// Start offset of the innermost construct that introduces names around
/// `node`, which is the granularity at which the set of names in scope
/// changes and therefore the granularity at which a context can be reused.
///
/// A lambda counts: its parameters are in scope only inside its body, so a
/// context built at the enclosing statement would not have them, and every
/// name in a lambda body would look undefined.
fn statement_start(node: Node<'_>) -> u32 {
    for ancestor in ast::ancestors(node) {
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if matches!(
            parent.kind(),
            "block"
                | "constructor_body"
                | "class_body"
                | "interface_body"
                | "enum_body_declarations"
                | "switch_block"
                | "switch_block_statement_group"
                | "program"
                | "lambda_expression"
        ) {
            return ancestor.start_byte() as u32;
        }
    }
    node.start_byte() as u32
}

/// Whether any node in a subtree has one of the given kinds.
fn subtree_contains(root: Node<'_>, kinds: &[&str]) -> bool {
    let mut stack = vec![root];
    let mut cursor = root.walk();
    while let Some(node) = stack.pop() {
        if kinds.contains(&node.kind()) {
            return true;
        }
        // A nested class or lambda has its own return statements, which say
        // nothing about this method.
        if matches!(
            node.kind(),
            "class_body" | "lambda_expression" | "class_declaration"
        ) && node.id() != root.id()
        {
            continue;
        }
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

/// The outermost class of a binary name: `Outer$Inner$1` yields `Outer`.
fn top_level_of(binary: &str) -> &str {
    match binary.find('$') {
        Some(at) => &binary[..at],
        None => binary,
    }
}

fn is_statement(kind: &str) -> bool {
    matches!(
        kind,
        "expression_statement"
            | "local_variable_declaration"
            | "return_statement"
            | "if_statement"
            | "while_statement"
            | "for_statement"
            | "enhanced_for_statement"
            | "do_statement"
            | "try_statement"
            | "switch_expression"
            | "throw_statement"
            | "synchronized_statement"
            | "assert_statement"
            | "field_declaration"
            | "method_declaration"
            | "constructor_declaration"
    )
}

/// Renders a diagnostic range for tests and logs.
pub fn describe(diagnostic: &Diagnostic) -> String {
    format!(
        "{}:{} {}",
        diagnostic.range.start.line, diagnostic.range.start.character, diagnostic.message
    )
}

/// The code of a diagnostic, if it carries one.
pub fn code_of(diagnostic: &Diagnostic) -> Option<&str> {
    match &diagnostic.code {
        Some(NumberOrString::String(code)) => Some(code),
        _ => None,
    }
}

/// Range helper used by code actions to find the diagnostic under a cursor.
pub fn contains(range: &Range, position: lsp_types::Position) -> bool {
    (range.start.line, range.start.character) <= (position.line, position.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}

/// Re-exported so the server can render a method in a diagnostic message.
pub use render::method_declaration;

/// Whether a `switch` uses a Java 14 form: arrow rules, or a position that needs a value.
fn is_modern_switch(node: Node<'_>) -> bool {
    let has_rules = ast::field(node, "body")
        .is_some_and(|body| ast::named_children(body).any(|child| child.kind() == "switch_rule"));
    let in_value_position = node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "variable_declarator"
                | "assignment_expression"
                | "return_statement"
                | "argument_list"
                | "parenthesized_expression"
                | "binary_expression"
                | "ternary_expression"
                | "cast_expression"
                | "lambda_expression"
                | "array_initializer"
                | "yield_statement"
        )
    });
    has_rules || in_value_position
}

//! Completion.
//!
//! The cursor is classified first — member access, a bare name, a type
//! position, an import — and only then are candidates gathered, because what
//! is valid depends entirely on where the cursor is. A receiver's type is
//! resolved through the semantic layer, so `names.` offers the members of
//! `List<String>` with `E` already substituted.

use crate::convert;
use crate::render;
use java_semantic::infer::{Inferer, Receiver};
use java_semantic::members::{FoundField, FoundMethod, LookupOptions};
use java_semantic::name::{self, Name};
use java_semantic::scope::{self, Context, Resolved};
use java_semantic::symbols::{ClassKind, FileId, MethodData, Visibility};
use java_semantic::{Db, Type};
use java_syntax::ast;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionTextEdit,
    Documentation, InsertTextFormat, MarkupContent, MarkupKind, TextEdit,
};

/// Client behaviour the completion list has to respect.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionSettings {
    /// The client renders `$1`-style placeholders.
    pub snippets: bool,
    /// The client applies `additionalTextEdits`, so imports can be added.
    pub additional_edits: bool,
}

/// What the cursor is positioned to complete.
#[derive(Debug)]
enum Trigger {
    /// `receiver.pre`
    Member { dot: u32, prefix_start: u32 },
    /// A bare identifier prefix.
    Name { prefix_start: u32 },
    /// Inside an `import` statement.
    Import { start: u32 },
    /// After `new`, where only types make sense.
    New { prefix_start: u32 },
}

impl Trigger {
    fn prefix_start(&self) -> u32 {
        match self {
            Trigger::Member { prefix_start, .. }
            | Trigger::Name { prefix_start }
            | Trigger::New { prefix_start } => *prefix_start,
            Trigger::Import { start } => *start,
        }
    }
}

/// Classifies the cursor lexically.
///
/// Lexical rather than syntactic on purpose: at the moment completion is
/// requested the text is usually not valid Java, so the token stream is more
/// reliable than the tree for deciding *what kind* of completion is wanted.
fn classify(text: &str, offset: u32) -> Trigger {
    let bytes = text.as_bytes();
    let mut prefix_start = offset as usize;
    while prefix_start > 0 && is_identifier_byte(bytes[prefix_start - 1]) {
        prefix_start -= 1;
    }
    let prefix_start_u32 = prefix_start as u32;

    // Skip whitespace before the prefix to find the character that governs.
    let mut at = prefix_start;
    while at > 0 && (bytes[at - 1] as char).is_whitespace() {
        at -= 1;
    }
    if at > 0 && bytes[at - 1] == b'.' {
        if let Some(statement) = enclosing_import(text, offset) {
            return Trigger::Import { start: statement };
        }
        return Trigger::Member {
            dot: at as u32 - 1,
            prefix_start: prefix_start_u32,
        };
    }
    if let Some(statement) = enclosing_import(text, offset) {
        return Trigger::Import { start: statement };
    }
    if preceding_keyword(text, at) == Some("new") {
        return Trigger::New {
            prefix_start: prefix_start_u32,
        };
    }
    Trigger::Name {
        prefix_start: prefix_start_u32,
    }
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// The keyword immediately before `at`, if the preceding word is one.
fn preceding_keyword(text: &str, at: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut end = at;
    while end > 0 && (bytes[end - 1] as char).is_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && bytes[start - 1].is_ascii_alphabetic() {
        start -= 1;
    }
    (start < end).then(|| &text[start..end])
}

/// The start of the `import` statement the cursor is inside, if any.
fn enclosing_import(text: &str, offset: u32) -> Option<u32> {
    let before = &text[..offset as usize];
    let line_start = before.rfind('\n').map(|at| at + 1).unwrap_or(0);
    let line = &text[line_start..offset as usize];
    let trimmed = line.trim_start();
    if !trimmed.starts_with("import") {
        return None;
    }
    // `import` on this line but already terminated is not a completion site.
    if line.contains(';') {
        return None;
    }
    let keyword_at = line_start + (line.len() - trimmed.len());
    let after_keyword = keyword_at + "import".len();
    let rest = text[after_keyword..offset as usize].trim_start();
    let rest = rest
        .strip_prefix("static")
        .map(str::trim_start)
        .unwrap_or(rest);
    Some((offset as usize - rest.len()) as u32)
}

/// Text that would let a file parse well enough to complete in, or `None`
/// when the file already parses well enough.
///
/// Error recovery sometimes collapses an entire class around a dangling `.`,
/// and a file can be *saved* in that state, so there is no earlier parse to
/// fall back on. Inserting a placeholder identifier at the cursor restores the
/// structure. The caller applies this to the document, asks for completions,
/// and restores the original text; it is deliberately a last resort, because
/// it costs two extra parses.
pub fn repair_for_completion(db: &Db, file: FileId, offset: u32) -> Option<String> {
    let source = db.source(file)?;
    let text = source.text();
    // Only when the cursor is not inside any class the resolver can see,
    // while the file clearly declares one.
    let outline = db.resolution_outline(file)?;
    if outline.class_at(offset).is_some() {
        return None;
    }
    if outline.classes.is_empty() && !declares_a_type(text) {
        return None;
    }
    let offset = offset.min(text.len() as u32) as usize;
    let mut patched = String::with_capacity(text.len() + REPAIR_NAME.len() + 1);
    patched.push_str(&text[..offset]);
    patched.push_str(REPAIR_NAME);
    // A statement needs its terminator, or recovery fails again.
    let rest = text[offset..].trim_start();
    if !rest.starts_with([';', ')', ',', ']']) {
        patched.push(';');
    }
    patched.push_str(&text[offset..]);
    Some(patched)
}

/// Placeholder inserted by `repair_for_completion`. It is never offered as a
/// completion because the prefix the client sees comes from the original text.
const REPAIR_NAME: &str = "__ferrisjdt_completion__";

fn declares_a_type(text: &str) -> bool {
    ["class ", "interface ", "enum ", "@interface "]
        .iter()
        .any(|keyword| text.contains(keyword))
}

/// Completes at a position, repairing the document first when the parse there
/// has collapsed.
///
/// The repair is applied to the document and undone before returning, so
/// nothing else observes it. This is the entry point a server should call;
/// [`completions`] is the pure form, for a document already in a usable state.
pub fn completions_repairing(
    db: &mut Db,
    file: FileId,
    offset: u32,
    settings: CompletionSettings,
) -> Vec<CompletionItem> {
    let Some(repaired) = repair_for_completion(db, file, offset) else {
        return completions(db, file, offset, settings);
    };
    let Some(original) = db.text(file) else {
        return completions(db, file, offset, settings);
    };
    let version = db.version(file).unwrap_or(0);
    if !db.edit_document(file, version, |source| source.set_text(repaired, version)) {
        return completions(db, file, offset, settings);
    }
    let items = completions(db, file, offset, settings);
    db.edit_document(file, version, |source| {
        source.set_text(original.to_string(), version)
    });
    items
}

/// Produces the completion list for a position.
pub fn completions(
    db: &Db,
    file: FileId,
    offset: u32,
    settings: CompletionSettings,
) -> Vec<CompletionItem> {
    let Some(source) = db.source(file) else {
        return Vec::new();
    };
    let text = source.text();
    let offset = offset.min(text.len() as u32);
    let trigger = classify(text, offset);
    let prefix = text[trigger.prefix_start() as usize..offset as usize].to_string();

    // For a member access the receiver's scope is what matters, and it is not
    // always the cursor's: a half-typed `a.` inside a lambda leaves the dot
    // outside the lambda in the tree, so a context built at the cursor would
    // not have the lambda's parameters in it.
    let receiver = match &trigger {
        Trigger::Member { dot, .. } => ast::expression_ending_at(source.tree(), *dot),
        _ => None,
    };
    let context_offset = receiver
        .map(|expression| expression.start_byte() as u32)
        .unwrap_or(offset);
    let Some(context) = scope::context_at(db, file, context_offset) else {
        return Vec::new();
    };
    let mut builder = Builder {
        db,
        file,
        context: &context,
        settings,
        prefix: &prefix,
        replace: (trigger.prefix_start(), offset),
        items: Vec::new(),
    };

    match &trigger {
        Trigger::Member { .. } => {
            if let Some(expression) = receiver {
                builder.member_completions(expression);
            }
        }
        Trigger::New { .. } => {
            builder.type_completions(true);
        }
        Trigger::Import { start } => {
            let written = text[*start as usize..offset as usize].to_string();
            builder.import_completions(&written);
        }
        Trigger::Name { .. } => {
            if in_type_position(&context, offset) {
                builder.type_completions(false);
            } else {
                builder.scope_completions();
                builder.type_completions(false);
                builder.keyword_completions();
            }
        }
    }
    builder.items
}

/// True where only a type name is valid: after `extends`, `implements`,
/// `throws`, `new`, or `instanceof`.
fn in_type_position(context: &Context<'_>, offset: u32) -> bool {
    let text = context.text();
    let mut at = offset as usize;
    let bytes = text.as_bytes();
    while at > 0 && is_identifier_byte(bytes[at - 1]) {
        at -= 1;
    }
    matches!(
        preceding_keyword(text, at),
        Some("extends" | "implements" | "throws" | "instanceof")
    )
}

struct Builder<'a> {
    db: &'a Db,
    file: FileId,
    context: &'a Context<'a>,
    settings: CompletionSettings,
    prefix: &'a str,
    /// Range the inserted text replaces.
    replace: (u32, u32),
    items: Vec<CompletionItem>,
}

/// Ordering groups. A smaller number sorts first, which is how locals end up
/// above inherited `java.lang.Object` methods.
mod rank {
    pub const LOCAL: u8 = 0;
    pub const FIELD: u8 = 1;
    pub const METHOD: u8 = 2;
    pub const INHERITED: u8 = 4;
    pub const FROM_OBJECT: u8 = 6;
    pub const TYPE: u8 = 7;
    pub const PACKAGE: u8 = 8;
    pub const KEYWORD: u8 = 9;
}

impl<'a> Builder<'a> {
    fn matches(&self, label: &str) -> bool {
        matches_prefix(label, self.prefix)
    }

    fn text_edit(&self, new_text: String) -> Option<CompletionTextEdit> {
        let range = convert::range_of(self.db, self.file, self.replace)?;
        Some(CompletionTextEdit::Edit(TextEdit { range, new_text }))
    }

    fn push(&mut self, mut item: CompletionItem, rank: u8) {
        // Clients sort by sortText lexicographically, so the group number is
        // written first and the label breaks ties within a group.
        item.sort_text = Some(format!("{rank}{}", item.label.to_lowercase()));
        if item.filter_text.is_none() {
            item.filter_text = Some(item.label.clone());
        }
        self.items.push(item);
    }

    fn member_completions(&mut self, expression: java_syntax::Node<'_>) {
        let inferer = Inferer::new(self.context);
        let Some(receiver) = inferer.receiver_of(expression, 0) else {
            return;
        };
        match receiver {
            Receiver::Value(ty) => self.value_members(&inferer, &ty, LookupOptions::instance()),
            Receiver::Super(ty) => self.value_members(&inferer, &ty, LookupOptions::default()),
            Receiver::StaticClass(binary) => self.static_members(&binary),
            Receiver::Package(package) => self.package_members(&package),
        }
    }

    fn value_members(&mut self, inferer: &Inferer<'_>, ty: &Type, options: LookupOptions) {
        if ty.is_unknown() {
            return;
        }
        let members = inferer.members_for(ty, options);
        for found in &members.fields {
            if !self.matches(&found.field.name) {
                continue;
            }
            let item = self.field_item(found);
            let rank = match found.depth {
                0 => rank::FIELD,
                _ => rank::INHERITED,
            };
            self.push(item, rank);
        }
        for found in &members.methods {
            if !self.matches(&found.method.name) {
                continue;
            }
            let item = self.method_item(found);
            self.push(item, method_rank(found));
        }
    }

    fn static_members(&mut self, binary: &Name) {
        let ty = Type::class(binary.clone());
        let inferer = Inferer::new(self.context);
        let members = inferer.members_for(&ty, LookupOptions::statics());
        for found in &members.fields {
            if self.matches(&found.field.name) {
                let item = self.field_item(found);
                self.push(item, rank::FIELD);
            }
        }
        for found in &members.methods {
            if self.matches(&found.method.name) {
                let item = self.method_item(found);
                self.push(item, method_rank(found));
            }
        }
        // Nested types are reached through the outer class name.
        for nested in &members.nested {
            let simple = name::simple_name(nested);
            if self.matches(simple) && !name::is_internal(nested) {
                let item = self.type_item(nested, false);
                self.push(item, rank::TYPE);
            }
        }
    }

    fn package_members(&mut self, package: &str) {
        for binary in self.db.classes_in_package(package) {
            let simple = name::simple_name(&binary);
            if self.matches(simple) && !name::is_internal(&binary) {
                let item = self.type_item(&binary, false);
                self.push(item, rank::TYPE);
            }
        }
        for sub in self.db.subpackages(package) {
            if self.matches(&sub) {
                let item = CompletionItem {
                    label: sub.to_string(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some(format!("package {package}.{sub}")),
                    text_edit: self.text_edit(sub.to_string()),
                    ..Default::default()
                };
                self.push(item, rank::PACKAGE);
            }
        }
    }

    fn import_completions(&mut self, written: &str) {
        // `import java.util.|` completes within the package named so far.
        let (package, prefix) = match written.rfind('.') {
            Some(at) => (&written[..at], &written[at + 1..]),
            None => ("", written),
        };
        // Only the last segment is replaced; the qualifier stays as typed.
        self.replace = (self.replace.1 - prefix.len() as u32, self.replace.1);
        for binary in self.db.classes_in_package(package) {
            let simple = name::simple_name(&binary);
            if matches_prefix(simple, prefix) && !name::is_internal(&binary) {
                let item = CompletionItem {
                    label: simple.to_string(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(name::source_name(&binary)),
                    text_edit: self.text_edit(simple.to_string()),
                    ..Default::default()
                };
                self.push(item, rank::TYPE);
            }
        }
        for sub in self.db.subpackages(package) {
            if matches_prefix(&sub, prefix) {
                let item = CompletionItem {
                    label: sub.to_string(),
                    kind: Some(CompletionItemKind::MODULE),
                    detail: Some("package".to_string()),
                    text_edit: self.text_edit(sub.to_string()),
                    ..Default::default()
                };
                self.push(item, rank::PACKAGE);
            }
        }
    }

    /// Locals, parameters, and the enclosing classes' own members.
    fn scope_completions(&mut self) {
        for local in &self.context.locals {
            if !self.matches(&local.name) {
                continue;
            }
            let item = CompletionItem {
                label: local.name.to_string(),
                // The protocol has no distinct kind for a parameter in a
                // completion list; the distinction is in the label details.
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(local.ty.display_short()),
                label_details: Some(CompletionItemLabelDetails {
                    detail: Some(format!(" {}", local.ty.display_short())),
                    description: Some(
                        if local.is_parameter {
                            "parameter"
                        } else {
                            "local"
                        }
                        .to_string(),
                    ),
                }),
                text_edit: self.text_edit(local.name.to_string()),
                ..Default::default()
            };
            self.push(item, rank::LOCAL);
        }

        let inferer = Inferer::new(self.context);
        for (depth, enclosing) in self.context.enclosing.clone().iter().enumerate() {
            let Some(class) = self.db.class(enclosing) else {
                continue;
            };
            let self_type = java_semantic::lower::self_type_of(&class);
            let members = java_semantic::members::members_of(
                self.db,
                &self_type,
                LookupOptions {
                    include_private: true,
                    include_instance: !self.context.in_static_context || depth > 0,
                    ..LookupOptions::default()
                },
            );
            for found in &members.fields {
                if self.matches(&found.field.name) {
                    let item = self.field_item(found);
                    self.push(
                        item,
                        if found.depth == 0 {
                            rank::FIELD
                        } else {
                            rank::INHERITED
                        },
                    );
                }
            }
            for found in &members.methods {
                if self.matches(&found.method.name) {
                    let item = self.method_item(found);
                    self.push(item, method_rank(found));
                }
            }
        }
        let _ = inferer;

        for (var, bounds) in &self.context.type_vars {
            if self.matches(var) {
                let item = CompletionItem {
                    label: var.to_string(),
                    kind: Some(CompletionItemKind::TYPE_PARAMETER),
                    detail: Some(match bounds.first() {
                        Some(bound) => format!("{var} extends {}", bound.display_short()),
                        None => format!("type parameter {var}"),
                    }),
                    text_edit: self.text_edit(var.to_string()),
                    ..Default::default()
                };
                self.push(item, rank::TYPE);
            }
        }
    }

    /// Type names reachable by simple name, plus importable ones.
    fn type_completions(&mut self, constructors: bool) {
        if self.prefix.is_empty() && !constructors {
            // Offering every class on the classpath for an empty prefix would
            // bury the useful candidates; wait for at least one character.
            self.local_type_completions(constructors);
            return;
        }
        self.local_type_completions(constructors);

        let prefix = self.prefix.to_string();
        let mut offered = 0usize;
        // Classpath types, which may need an import.
        for binary in self.db.classpath().all_names() {
            if offered >= MAX_IMPORTABLE_TYPES {
                break;
            }
            let simple = name::simple_name(binary);
            if !matches_prefix(simple, &prefix) || name::is_internal(binary) {
                continue;
            }
            // Nested classes are only offered through their outer name.
            if name::enclosing_class(binary).is_some() {
                continue;
            }
            let binary = binary.to_string();
            let item = self.type_item(&binary, constructors);
            self.push(item, rank::TYPE);
            offered += 1;
        }
    }

    /// Types already usable by simple name: this file, this package, imports.
    fn local_type_completions(&mut self, constructors: bool) {
        let outline = self.context.outline.clone();
        let mut candidates: Vec<Name> = Vec::new();
        for class in &outline.classes {
            candidates.push(class.binary.clone());
        }
        for import in &outline.imports {
            if import.is_static {
                continue;
            }
            if import.is_wildcard {
                candidates.extend(self.db.classes_in_package(&import.path));
            } else {
                candidates.push(import.path.clone());
            }
        }
        candidates.extend(self.db.classes_in_package(self.context.package()));
        if !self.prefix.is_empty() {
            candidates.extend(self.db.classes_in_package("java.lang"));
        }
        candidates.sort();
        candidates.dedup();
        for binary in candidates {
            let simple = name::simple_name(&binary);
            if !self.matches(simple) || name::is_internal(&binary) {
                continue;
            }
            if self.db.class(&binary).is_none() {
                continue;
            }
            let item = self.type_item(&binary, constructors);
            self.push(item, rank::TYPE);
        }
    }

    fn keyword_completions(&mut self) {
        const KEYWORDS: &[&str] = &[
            "abstract",
            "assert",
            "boolean",
            "break",
            "byte",
            "case",
            "catch",
            "char",
            "class",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extends",
            "final",
            "finally",
            "float",
            "for",
            "if",
            "implements",
            "import",
            "instanceof",
            "int",
            "interface",
            "long",
            "new",
            "null",
            "package",
            "private",
            "protected",
            "public",
            "return",
            "short",
            "static",
            "super",
            "switch",
            "synchronized",
            "this",
            "throw",
            "throws",
            "transient",
            "try",
            "void",
            "volatile",
            "while",
            "true",
            "false",
        ];
        for keyword in KEYWORDS {
            if self.matches(keyword) {
                let item = CompletionItem {
                    label: keyword.to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    text_edit: self.text_edit(keyword.to_string()),
                    ..Default::default()
                };
                self.push(item, rank::KEYWORD);
            }
        }
    }

    fn field_item(&self, found: &FoundField) -> CompletionItem {
        let ty = found.field.ty.display_short();
        CompletionItem {
            label: found.field.name.to_string(),
            kind: Some(
                if found.field.modifiers.is_static && found.field.modifiers.is_final {
                    CompletionItemKind::CONSTANT
                } else {
                    CompletionItemKind::FIELD
                },
            ),
            detail: Some(ty.clone()),
            label_details: Some(CompletionItemLabelDetails {
                detail: Some(format!(" {ty}")),
                description: Some(name::simple_name(&found.owner.name).to_string()),
            }),
            documentation: found.field.doc.as_deref().map(markdown),
            deprecated: Some(found.field.modifiers.is_deprecated),
            text_edit: self.text_edit(found.field.name.to_string()),
            ..Default::default()
        }
    }

    fn method_item(&self, found: &FoundMethod) -> CompletionItem {
        let method = &found.method;
        let (new_text, format) = self.method_insert_text(method);
        CompletionItem {
            label: method.name.to_string(),
            kind: Some(if method.is_constructor {
                CompletionItemKind::CONSTRUCTOR
            } else {
                CompletionItemKind::METHOD
            }),
            detail: Some(format!(
                "{} \u{2192} {}",
                render::parameter_list(method),
                method.ret.display_short()
            )),
            label_details: Some(CompletionItemLabelDetails {
                detail: Some(render::parameter_list(method)),
                description: Some(format!(
                    "{} \u{b7} {}",
                    method.ret.display_short(),
                    name::simple_name(&found.owner.name)
                )),
            }),
            documentation: method.doc.as_deref().map(markdown),
            deprecated: Some(method.modifiers.is_deprecated),
            insert_text_format: Some(format),
            text_edit: self.text_edit(new_text),
            ..Default::default()
        }
    }

    /// Inserted text for a method. With snippet support the parameters become
    /// tab stops named after the declaration, which is what makes a completed
    /// call immediately editable.
    fn method_insert_text(&self, method: &MethodData) -> (String, InsertTextFormat) {
        if method.params.is_empty() {
            return (format!("{}()", method.name), InsertTextFormat::PLAIN_TEXT);
        }
        if !self.settings.snippets {
            return (format!("{}(", method.name), InsertTextFormat::PLAIN_TEXT);
        }
        let placeholders: Vec<String> = method
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| format!("${{{}:{}}}", index + 1, param.name))
            .collect();
        (
            format!("{}({})", method.name, placeholders.join(", ")),
            InsertTextFormat::SNIPPET,
        )
    }

    fn type_item(&self, binary: &str, constructor: bool) -> CompletionItem {
        let simple = name::simple_name(binary);
        let class = self.db.class(binary);
        let kind = match class.as_ref().map(|c| c.kind) {
            Some(ClassKind::Interface) => CompletionItemKind::INTERFACE,
            Some(ClassKind::Enum) => CompletionItemKind::ENUM,
            Some(ClassKind::Annotation) => CompletionItemKind::INTERFACE,
            _ => CompletionItemKind::CLASS,
        };
        // `new ArrayList<` leaves the diamond for the client to complete.
        let insert = if constructor {
            let generic = class
                .as_ref()
                .map(|c| !c.type_params.is_empty())
                .unwrap_or(false);
            if generic {
                format!("{simple}<>(")
            } else {
                format!("{simple}(")
            }
        } else {
            simple.to_string()
        };
        let additional = if self.settings.additional_edits {
            crate::imports::import_edit(self.db, self.file, binary).map(|edit| vec![edit])
        } else {
            None
        };
        CompletionItem {
            label: simple.to_string(),
            kind: Some(kind),
            detail: Some(name::source_name(binary)),
            label_details: Some(CompletionItemLabelDetails {
                detail: None,
                description: Some(name::package_of(binary).to_string()),
            }),
            documentation: class.as_ref().and_then(|c| c.doc.as_deref()).map(markdown),
            deprecated: class.as_ref().map(|c| c.modifiers.is_deprecated),
            additional_text_edits: additional,
            text_edit: self.text_edit(insert),
            ..Default::default()
        }
    }
}

/// Cap on importable classpath types offered for one prefix. Without a cap a
/// one-letter prefix would produce thousands of items and the client would
/// spend longer rendering them than the server spent finding them.
const MAX_IMPORTABLE_TYPES: usize = 200;

fn method_rank(found: &FoundMethod) -> u8 {
    if found.owner.name.as_ref() == "java.lang.Object" {
        return rank::FROM_OBJECT;
    }
    match found.depth {
        0 => rank::METHOD,
        _ => rank::INHERITED,
    }
}

/// Prefix matching: case-insensitive, and camel-case aware so `AL` finds
/// `ArrayList`.
pub fn matches_prefix(label: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if label.len() < prefix.len() {
        return false;
    }
    if label
        .as_bytes()
        .iter()
        .zip(prefix.as_bytes())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return true;
    }
    // Camel humps: every uppercase letter of the prefix must match a hump in
    // order, which is only attempted when the prefix is all uppercase.
    if !prefix.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    let mut humps = label.chars().filter(|c| c.is_ascii_uppercase());
    prefix.chars().all(|wanted| humps.next() == Some(wanted))
}

fn markdown(text: &str) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: text.to_string(),
    })
}

/// Visibility filter for members offered from outside their declaring class.
pub fn is_offerable(visibility: Visibility, same_package: bool) -> bool {
    match visibility {
        Visibility::Public | Visibility::Protected => true,
        Visibility::Package => same_package,
        Visibility::Private => false,
    }
}

/// Resolves the documentation and detail of a selected item, for
/// `completionItem/resolve`. Only the parts that are expensive to compute for
/// every item are filled in here.
pub fn resolve(db: &Db, mut item: CompletionItem) -> CompletionItem {
    if item.documentation.is_some() {
        return item;
    }
    // A type's javadoc is the one piece worth fetching lazily: it requires
    // decoding the class, which the list deliberately avoids for every entry.
    let Some(detail) = item.detail.clone() else {
        return item;
    };
    if let Some(class) = db
        .class(&detail.replace('.', "$"))
        .or_else(|| db.class(&detail))
    {
        if let Some(doc) = class.doc.as_deref() {
            item.documentation = Some(markdown(doc));
        }
    }
    item
}

/// Resolution result used by hover and definition for a name under the cursor.
pub fn resolved_at(context: &Context<'_>, name: &str) -> Option<Resolved> {
    context.resolve_name(name)
}

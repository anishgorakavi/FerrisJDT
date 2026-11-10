//! Identifying the declaration a cursor refers to.
//!
//! Definition, references, rename, and highlights all need the same answer,
//! and they need it to be an *identity* rather than a name: a reference is a
//! token that resolves to the same declaration, not a token that happens to
//! spell the same word. That distinction is what keeps rename from renaming
//! an unrelated `size` in another class.

use java_semantic::infer::{Inferer, Receiver};
use java_semantic::members::LookupOptions;
use java_semantic::name::{self, Name};
use java_semantic::scope::{self, Context, Resolved};
use java_semantic::symbols::{FileId, Origin};
use java_semantic::{Db, Type};
use java_syntax::{ast, Node};

/// A declaration, identified so that two references can be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A local variable or parameter, identified by where it is declared.
    Local {
        file: FileId,
        name_range: (u32, u32),
        name: Name,
    },
    Field {
        owner: Name,
        name: Name,
    },
    Method {
        owner: Name,
        name: Name,
        /// Position of the declaration in the owner's method list, which
        /// distinguishes overloads that share a name and an arity.
        index: u32,
    },
    Class(Name),
    TypeParam {
        owner: Name,
        name: Name,
    },
    Package(String),
}

impl Target {
    /// The identifier text a reference to this declaration must have.
    pub fn name(&self) -> &str {
        match self {
            Target::Local { name, .. } => name,
            Target::Field { name, .. } => name,
            Target::Method { name, .. } => name,
            Target::Class(binary) => name::simple_name(binary),
            Target::TypeParam { name, .. } => name,
            Target::Package(package) => package,
        }
    }

    /// Whether references can only occur in the declaring file.
    pub fn is_file_local(&self) -> bool {
        matches!(self, Target::Local { .. } | Target::TypeParam { .. })
    }

    /// The declaration's origin, for navigation.
    pub fn origin(&self, db: &Db) -> Option<Origin> {
        match self {
            Target::Local {
                file, name_range, ..
            } => Some(Origin::Source {
                file: *file,
                range: *name_range,
                name_range: *name_range,
            }),
            Target::Field { owner, name } => {
                let class = db.class(owner)?;
                class.find_field(name).map(|f| f.origin.clone())
            }
            Target::Method { owner, index, .. } => {
                let class = db.class(owner)?;
                class
                    .methods
                    .get(*index as usize)
                    .map(|method| method.origin.clone())
            }
            Target::Class(binary) => db.class(binary).map(|c| c.origin.clone()),
            Target::TypeParam { owner, .. } => db.class(owner).map(|c| c.origin.clone()),
            Target::Package(_) => None,
        }
    }

    /// A short description for hover and for the rename prompt.
    pub fn describe(&self, db: &Db) -> Option<String> {
        match self {
            Target::Local { name, .. } => Some(name.to_string()),
            Target::Field { owner, name } => {
                let class = db.class(owner)?;
                let field = class.find_field(name)?;
                Some(crate::render::field_declaration(field))
            }
            Target::Method { owner, index, .. } => {
                let class = db.class(owner)?;
                let method = class.methods.get(*index as usize)?;
                Some(crate::render::method_declaration(method))
            }
            Target::Class(binary) => {
                let class = db.class(binary)?;
                Some(crate::render::class_declaration(&class))
            }
            Target::TypeParam { name, .. } => Some(format!("type parameter {name}")),
            Target::Package(package) => Some(format!("package {package}")),
        }
    }
}

/// The identifier token the cursor is on, together with its byte range.
pub fn identifier_at(db: &Db, file: FileId, offset: u32) -> Option<((u32, u32), String)> {
    let source = db.source(file)?;
    let node = ast::identifier_at(source.tree(), offset)?;
    Some((
        (node.start_byte() as u32, node.end_byte() as u32),
        ast::text(node, source.text()).to_string(),
    ))
}

/// What the cursor refers to.
pub fn target_at(db: &Db, file: FileId, offset: u32) -> Option<Target> {
    let context = scope::context_at(db, file, offset)?;
    let node = ast::identifier_at(context.source.tree(), offset)?;
    target_of_node(&context, node)
}

/// What an identifier node refers to, in a known context.
pub fn target_of_node(context: &Context<'_>, node: Node<'_>) -> Option<Target> {
    let db = context.db;
    let text = context.text();
    let written = ast::text(node, text);
    let parent = node.parent();
    let inferer = Inferer::new(context);

    if let Some(parent) = parent {
        let is_field = |field: &str| ast::field(parent, field).map(|n| n.id()) == Some(node.id());
        match parent.kind() {
            // A call: resolve the overload so the right declaration is named.
            "method_invocation" if is_field("name") => {
                if let Some(found) = inferer.resolve_invocation(parent) {
                    return Some(Target::Method {
                        owner: found.owner.name.clone(),
                        name: found.method.name.clone(),
                        index: found.index,
                    });
                }
                // Not resolvable, but the receiver might still be known.
                return None;
            }
            "field_access" if is_field("field") => {
                let object = ast::field(parent, "object")?;
                let receiver = inferer.receiver_of(object, 0)?;
                return field_target(context, &receiver, written);
            }
            // The declaration itself.
            "variable_declarator" if is_field("name") => {
                let grandparent = parent.parent()?;
                return match grandparent.kind() {
                    "field_declaration" | "constant_declaration" => Some(Target::Field {
                        owner: context.enclosing.first()?.clone(),
                        name: name::name(written),
                    }),
                    _ => Some(Target::Local {
                        file: context.file,
                        name_range: (node.start_byte() as u32, node.end_byte() as u32),
                        name: name::name(written),
                    }),
                };
            }
            "formal_parameter"
            | "catch_formal_parameter"
            | "enhanced_for_statement"
            | "spread_parameter"
            | "resource"
                if is_field("name") =>
            {
                return Some(Target::Local {
                    file: context.file,
                    name_range: (node.start_byte() as u32, node.end_byte() as u32),
                    name: name::name(written),
                });
            }
            "method_declaration" | "constructor_declaration" if is_field("name") => {
                let owner = context.enclosing.first()?.clone();
                let class = db.class(&owner)?;
                // The declaration is found by its name's position, which is
                // exact even when several overloads share a name.
                let index = class
                    .methods
                    .iter()
                    .position(|m| {
                        m.origin
                            .name_range()
                            .is_some_and(|r| r.0 == node.start_byte() as u32)
                    })
                    .or_else(|| class.methods.iter().position(|m| &*m.name == written))?;
                return Some(Target::Method {
                    owner,
                    name: class.methods[index].name.clone(),
                    index: index as u32,
                });
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration"
                if is_field("name") =>
            {
                let declared = context
                    .outline
                    .classes
                    .iter()
                    .find(|c| c.name_range.0 == node.start_byte() as u32)?;
                return Some(Target::Class(declared.binary.clone()));
            }
            "enum_constant" if is_field("name") => {
                return Some(Target::Field {
                    owner: context.enclosing.first()?.clone(),
                    name: name::name(written),
                });
            }
            "type_parameter" => {
                return Some(Target::TypeParam {
                    owner: context.enclosing.first()?.clone(),
                    name: name::name(written),
                });
            }
            // A qualified name: resolve the whole dotted form.
            "scoped_type_identifier" | "scoped_identifier" => {
                let qualified = qualified_up_to(parent, node, text);
                if let Some(binary) = context.type_scope().resolve_class_name(&qualified) {
                    return Some(Target::Class(binary));
                }
                if db.is_package(&qualified) {
                    return Some(Target::Package(qualified));
                }
            }
            _ => {}
        }
    }

    // A plain name: locals, then fields, then types, then packages.
    match context.resolve_name(written)? {
        Resolved::Local(local) => Some(Target::Local {
            file: context.file,
            name_range: local.name_range,
            name: local.name,
        }),
        Resolved::Field { owner, field, .. } => Some(Target::Field {
            owner: owner.name.clone(),
            name: field.name.clone(),
        }),
        Resolved::Class(binary) => Some(Target::Class(binary)),
        Resolved::TypeVar(var) => Some(Target::TypeParam {
            owner: var.owner.unwrap_or_else(|| name::name("")),
            name: var.name,
        }),
        Resolved::Package(package) => Some(Target::Package(package)),
    }
}

/// The dotted name formed by the segments of `scoped` up to and including
/// `node`, which is what the cursor is pointing at within a qualified name.
fn qualified_up_to(scoped: Node<'_>, node: Node<'_>, text: &str) -> String {
    let full = java_semantic::outline::dotted(scoped, text);
    let end = node.end_byte();
    // Count how many segments end at or before the cursor's token.
    let mut taken = Vec::new();
    for segment in full.split('.') {
        taken.push(segment);
        let rebuilt = taken.join(".");
        if scoped.start_byte() + rebuilt.len() >= end {
            break;
        }
    }
    taken.join(".")
}

fn field_target(context: &Context<'_>, receiver: &Receiver, written: &str) -> Option<Target> {
    let db = context.db;
    let inferer = Inferer::new(context);
    match receiver {
        Receiver::Value(ty) | Receiver::Super(ty) => {
            let members = inferer.members_for(ty, LookupOptions::instance());
            let found = members.field(written)?;
            Some(Target::Field {
                owner: found.owner.name.clone(),
                name: found.field.name.clone(),
            })
        }
        Receiver::StaticClass(binary) => {
            let members = java_semantic::members::members_of(
                db,
                &Type::class(binary.clone()),
                LookupOptions::default(),
            );
            match members.field(written) {
                Some(found) => Some(Target::Field {
                    owner: found.owner.name.clone(),
                    name: found.field.name.clone(),
                }),
                // `Outer.Inner` is a nested type, not a field.
                None => java_semantic::resolve::member_type(db, binary, written).map(Target::Class),
            }
        }
        Receiver::Package(package) => {
            let candidate = format!("{package}.{written}");
            if db.has_class(&candidate) {
                return Some(Target::Class(name::name(&candidate)));
            }
            db.is_package(&candidate)
                .then_some(Target::Package(candidate))
        }
    }
}

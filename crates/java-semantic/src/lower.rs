//! Lowering a parsed Java file into `ClassData`.
//!
//! This is where source declarations acquire resolved types. It runs per file
//! and is cached; a file is re-lowered when its own text changes or when the
//! set of classes in the workspace changes, never on an unrelated edit.

use crate::db::Db;
use crate::javadoc;
use crate::name::{self, Name};
use crate::outline::{self, FileOutline, OutlineClass};
use crate::resolve::Scope;
use crate::symbols::{
    ClassData, ClassKind, ClassRef, FieldData, MethodData, Modifiers, Origin, ParamData, TypeParam,
    Visibility,
};
use crate::types::{Type, TypeVar};
use java_syntax::{ast, Node, SourceFile};
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// Lowers every class declared in one file, outermost first.
pub fn lower_file(db: &Db, source: &SourceFile, outline: &FileOutline) -> Vec<ClassRef> {
    let text = source.text();
    let mut nodes: FxHashMap<u32, usize> = FxHashMap::default();
    for (index, class) in outline.classes.iter().enumerate() {
        nodes.insert(class.range.0, index);
    }
    let mut found: Vec<Option<Node<'_>>> = vec![None; outline.classes.len()];
    collect_declaration_nodes(source.tree().root_node(), &nodes, &mut found);

    // Type parameters of enclosing classes stay in scope inside their body.
    let mut scopes: Vec<Vec<(Name, Vec<Type>)>> = vec![Vec::new(); outline.classes.len()];
    let mut lowered = Vec::with_capacity(outline.classes.len());
    for (index, class) in outline.classes.iter().enumerate() {
        let Some(node) = found[index] else {
            // The declaration did not survive error recovery; skip it rather
            // than invent an empty class.
            continue;
        };
        let inherited = match class.parent {
            // A static nested class cannot use the outer class's parameters.
            Some(parent) if !class.is_static => scopes[parent as usize].clone(),
            _ => Vec::new(),
        };
        let mut scope = Scope {
            db,
            outline,
            enclosing: enclosing_chain(outline, index),
            type_vars: inherited,
        };
        let type_params = lower_type_params(node, text, &mut scope);
        scopes[index] = scope.type_vars.clone();
        let mut data = lower_class(class, node, text, &scope, type_params, outline);
        let generated = crate::lombok::augment(&mut data, node, text, class, outline, db);
        lowered.push(Arc::new(data));
        lowered.extend(generated.into_iter().map(Arc::new));
    }
    lowered
}

/// Binary names of `index` and each of its enclosing classes, innermost first.
fn enclosing_chain(outline: &FileOutline, index: usize) -> Vec<Name> {
    let mut chain = Vec::new();
    let mut current = Some(index as u32);
    while let Some(at) = current {
        let class = &outline.classes[at as usize];
        chain.push(class.binary.clone());
        current = class.parent;
    }
    chain
}

fn collect_declaration_nodes<'t>(
    node: Node<'t>,
    wanted: &FxHashMap<u32, usize>,
    out: &mut [Option<Node<'t>>],
) {
    if let Some(&index) = wanted.get(&(node.start_byte() as u32)) {
        // An anonymous class is recorded at the `new` expression that declares
        // it, so that node counts as a declaration here too.
        if outline::kind_of(node.kind()).is_some()
            || matches!(node.kind(), "object_creation_expression" | "enum_constant")
        {
            out[index] = Some(node);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_declaration_nodes(child, wanted, out);
    }
}

/// Lowers a `type_parameters` list and registers each parameter in `scope`.
fn lower_type_params(node: Node<'_>, text: &str, scope: &mut Scope<'_>) -> Vec<TypeParam> {
    let Some(list) = ast::field(node, "type_parameters") else {
        return Vec::new();
    };
    let declarations: Vec<Node<'_>> = ast::named_children(list)
        .filter(|c| c.kind() == "type_parameter")
        .collect();
    // Register the names before lowering bounds so that a bound may refer to
    // any parameter in the list, including itself.
    let first_new = scope.type_vars.len();
    for declaration in &declarations {
        if let Some(name_node) = ast::named_children(*declaration)
            .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
        {
            scope
                .type_vars
                .push((name::name(ast::text(name_node, text)), Vec::new()));
        }
    }
    let mut params = Vec::with_capacity(declarations.len());
    for (offset, declaration) in declarations.iter().enumerate() {
        let slot = first_new + offset;
        if slot >= scope.type_vars.len() {
            break;
        }
        let bounds: Vec<Type> = ast::named_children(*declaration)
            .find(|c| c.kind() == "type_bound")
            .map(|bound| {
                ast::named_children(bound)
                    .map(|ty| scope.lower_type(ty, text))
                    .collect()
            })
            .unwrap_or_default();
        scope.type_vars[slot].1 = bounds.clone();
        params.push(TypeParam {
            name: scope.type_vars[slot].0.clone(),
            bounds,
        });
    }
    params
}

fn visibility_of(node: Node<'_>, text: &str, default: Visibility) -> Visibility {
    for (keyword, visibility) in [
        ("public", Visibility::Public),
        ("protected", Visibility::Protected),
        ("private", Visibility::Private),
    ] {
        if outline::has_modifier(node, text, keyword) {
            return visibility;
        }
    }
    default
}

fn modifiers_of(node: Node<'_>, text: &str) -> Modifiers {
    Modifiers {
        is_static: outline::has_modifier(node, text, "static"),
        is_final: outline::has_modifier(node, text, "final"),
        is_abstract: outline::has_modifier(node, text, "abstract"),
        is_synchronized: outline::has_modifier(node, text, "synchronized"),
        is_native: outline::has_modifier(node, text, "native"),
        is_transient: outline::has_modifier(node, text, "transient"),
        is_volatile: outline::has_modifier(node, text, "volatile"),
        is_default: outline::has_modifier(node, text, "default"),
        is_varargs: false,
        is_deprecated: has_annotation(node, text, "Deprecated"),
        is_synthetic: false,
    }
}

fn has_annotation(node: Node<'_>, text: &str, simple: &str) -> bool {
    let Some(modifiers) = ast::named_children(node).find(|n| n.kind() == "modifiers") else {
        return false;
    };
    ast::named_children(modifiers)
        .filter(|m| matches!(m.kind(), "marker_annotation" | "annotation"))
        .filter_map(|m| ast::field(m, "name"))
        .any(|n| name::simple_name(&outline::dotted(n, text)) == simple)
}

fn source_origin(
    file: crate::symbols::FileId,
    node: Node<'_>,
    name_node: Option<Node<'_>>,
) -> Origin {
    let name_range = name_node
        .map(|n| (n.start_byte() as u32, n.end_byte() as u32))
        .unwrap_or((node.start_byte() as u32, node.end_byte() as u32));
    Origin::Source {
        file,
        range: (node.start_byte() as u32, node.end_byte() as u32),
        name_range,
    }
}

fn lower_class(
    class: &OutlineClass,
    node: Node<'_>,
    text: &str,
    scope: &Scope<'_>,
    type_params: Vec<TypeParam>,
    outline: &FileOutline,
) -> ClassData {
    let file = outline.file;
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    let mut nested = Vec::new();
    // An anonymous class's body is an unnamed child rather than a `body` field.
    let body_node = ast::field(node, "body")
        .filter(|body| body.kind().ends_with("_body"))
        .or_else(|| ast::named_children(node).find(|c| c.kind() == "class_body"));

    let default_member_visibility = match class.kind {
        // Interface and annotation members are implicitly public.
        ClassKind::Interface | ClassKind::Annotation => Visibility::Public,
        _ => Visibility::Package,
    };

    if let Some(body) = body_node {
        let members: Vec<Node<'_>> = ast::named_children(body)
            .flat_map(|member| {
                if member.kind() == "enum_body_declarations" {
                    ast::named_children(member).collect::<Vec<_>>()
                } else {
                    vec![member]
                }
            })
            .collect();
        for member in members {
            match member.kind() {
                "field_declaration" => {
                    lower_field(
                        member,
                        text,
                        scope,
                        file,
                        default_member_visibility,
                        &mut fields,
                    );
                }
                "constant_declaration" => {
                    // An interface field: implicitly public static final.
                    let before = fields.len();
                    lower_field(member, text, scope, file, Visibility::Public, &mut fields);
                    for field in &mut fields[before..] {
                        field.modifiers.is_static = true;
                        field.modifiers.is_final = true;
                    }
                }
                "enum_constant" => {
                    if let Some(name_node) = ast::field(member, "name") {
                        fields.push(FieldData {
                            name: name::name(ast::text(name_node, text)),
                            ty: Type::class(class.binary.clone()),
                            visibility: Visibility::Public,
                            modifiers: Modifiers {
                                is_static: true,
                                is_final: true,
                                ..Modifiers::default()
                            },
                            origin: source_origin(file, member, Some(name_node)),
                            doc: javadoc::doc_for(member, text),
                            constant_value: None,
                        });
                    }
                }
                "method_declaration" | "annotation_type_element_declaration" => {
                    if let Some(method) = lower_method(
                        member,
                        text,
                        scope,
                        file,
                        default_member_visibility,
                        class,
                        false,
                    ) {
                        methods.push(method);
                    }
                }
                "constructor_declaration" => {
                    if let Some(method) = lower_method(
                        member,
                        text,
                        scope,
                        file,
                        default_member_visibility,
                        class,
                        true,
                    ) {
                        methods.push(method);
                    }
                }
                kind if outline::kind_of(kind).is_some() => {
                    if let Some(name_node) = ast::field(member, "name") {
                        nested.push(name::name(&format!(
                            "{}${}",
                            class.binary,
                            ast::text(name_node, text)
                        )));
                    }
                }
                _ => {}
            }
        }
    }

    if class.kind == ClassKind::Enum {
        let enum_type = Type::class(class.binary.clone());
        let implicit = |name: &str, params: Vec<ParamData>, ret: Type| MethodData {
            name: name::name(name),
            type_params: Vec::new(),
            params,
            ret,
            throws: Vec::new(),
            visibility: Visibility::Public,
            modifiers: Modifiers {
                is_static: true,
                ..Modifiers::default()
            },
            origin: Origin::Synthetic,
            doc: None,
            is_constructor: false,
        };
        methods.push(implicit(
            "values",
            Vec::new(),
            Type::Array(Box::new(enum_type.clone())),
        ));
        methods.push(implicit(
            "valueOf",
            vec![ParamData {
                name: name::name("name"),
                ty: Type::string(),
            }],
            enum_type,
        ));
    }
    let is_record = node.kind() == "record_declaration";
    if is_record {
        add_record_members(
            node,
            text,
            scope,
            file,
            visibility_of(node, text, Visibility::Package),
            &mut fields,
            &mut methods,
        );
    }

    // A class with no declared constructor has an implicit no-argument one.
    // An anonymous class has no name to construct it by, so it gets none.
    if class.kind == ClassKind::Class
        && !class.is_anonymous
        && !is_record
        && !methods.iter().any(|m| m.is_constructor)
    {
        methods.push(MethodData {
            name: class.simple.clone(),
            type_params: Vec::new(),
            params: Vec::new(),
            ret: Type::VOID,
            throws: Vec::new(),
            visibility: visibility_of(node, text, Visibility::Package),
            modifiers: Modifiers::default(),
            origin: Origin::Synthetic,
            doc: None,
            is_constructor: true,
        });
    }

    // An anonymous class extends or implements exactly the type named in the
    // `new` expression; which of the two depends on what that type is.
    if class.is_anonymous {
        // The written type keeps its arguments (`new Iterator<String>() {...}`), so inherited
        // members see `String` rather than the raw type's erasure.
        let base = match ast::field(node, "type") {
            Some(written) => {
                let lowered = scope.lower_type(written, text);
                // `new Base<>() {...}` takes its arguments from a context this layer does not
                // track; unknown arguments keep members honest where the raw type's erasure
                // would make `T` an `Object`.
                let is_diamond = ast::named_children(written)
                    .any(|c| c.kind() == "type_arguments" && ast::named_children(c).count() == 0);
                match &lowered {
                    Type::Class { name, args } if is_diamond && args.is_empty() => {
                        // A class of this file is still being lowered, so its outline answers.
                        let arity = match scope.outline.find(name) {
                            Some(declared) => declared.type_params.len(),
                            None => scope.db.class(name).map_or(0, |c| c.type_params.len()),
                        };
                        if arity == 0 {
                            lowered
                        } else {
                            Type::Class {
                                name: name.clone(),
                                args: vec![Type::Unknown; arity],
                            }
                        }
                    }
                    _ => lowered,
                }
            }
            None => class
                .anonymous_base
                .as_deref()
                .map(|written| scope.class_type(written, Vec::new()))
                .unwrap_or(Type::Unknown),
        };
        let base_is_interface = base
            .class_name()
            .and_then(|binary| scope.db.class(binary))
            .map(|declared| declared.is_interface())
            .unwrap_or(false);
        let (superclass, interfaces) = if base_is_interface {
            (Some(Type::object()), vec![base])
        } else {
            (Some(base), Vec::new())
        };
        return ClassData {
            name: class.binary.clone(),
            kind: ClassKind::Class,
            visibility: Visibility::Package,
            modifiers: Modifiers::default(),
            type_params,
            superclass,
            interfaces,
            fields,
            methods,
            nested,
            origin: source_origin(file, node, None),
            doc: None,
        };
    }

    let superclass = match class.kind {
        ClassKind::Interface | ClassKind::Annotation => None,
        ClassKind::Enum => Some(Type::generic(
            "java.lang.Enum",
            vec![Type::class(class.binary.clone())],
        )),
        ClassKind::Class if is_record => Some(Type::class("java.lang.Record")),
        ClassKind::Class => ast::named_children(node)
            .find(|c| c.kind() == "superclass")
            .and_then(|s| ast::named_children(s).next())
            .map(|ty| scope.lower_type(ty, text))
            // Every class but Object extends Object implicitly.
            .or_else(|| (&*class.binary != "java.lang.Object").then(Type::object)),
    };

    let mut interfaces: Vec<Type> = ast::named_children(node)
        .find(|c| matches!(c.kind(), "super_interfaces" | "extends_interfaces"))
        .and_then(|list| ast::named_children(list).find(|c| c.kind() == "type_list"))
        .map(|list| {
            ast::named_children(list)
                .map(|ty| scope.lower_type(ty, text))
                .collect()
        })
        .unwrap_or_default();
    // An annotation interface implicitly extends `java.lang.annotation.Annotation`.
    if class.kind == ClassKind::Annotation {
        interfaces.push(Type::class("java.lang.annotation.Annotation"));
    }

    ClassData {
        name: class.binary.clone(),
        kind: class.kind,
        visibility: visibility_of(node, text, Visibility::Package),
        modifiers: Modifiers {
            is_static: class.is_static,
            ..modifiers_of(node, text)
        },
        type_params,
        superclass,
        interfaces,
        fields,
        methods,
        nested,
        origin: source_origin(file, node, ast::field(node, "name")),
        doc: javadoc::doc_for(node, text),
    }
}

/// A record's components are private final fields, each with a public accessor of the same name,
/// and the canonical constructor takes them all unless the record declares one itself (a compact
/// constructor is the canonical one, so it counts as absent here).
fn add_record_members(
    node: Node<'_>,
    text: &str,
    scope: &Scope<'_>,
    file: crate::symbols::FileId,
    visibility: Visibility,
    fields: &mut Vec<FieldData>,
    methods: &mut Vec<MethodData>,
) {
    let Some(list) = ast::field(node, "parameters") else {
        return;
    };
    let mut params = Vec::new();
    let mut is_varargs = false;
    for component in ast::named_children(list) {
        let (name_node, ty) = match component.kind() {
            "formal_parameter" => (
                ast::field(component, "name"),
                ast::field(component, "type")
                    .map(|ty| scope.lower_type(ty, text))
                    .unwrap_or(Type::Unknown),
            ),
            "spread_parameter" => {
                is_varargs = true;
                let element = ast::named_children(component)
                    .find(|c| !matches!(c.kind(), "variable_declarator" | "modifiers"))
                    .map(|ty| scope.lower_type(ty, text))
                    .unwrap_or(Type::Unknown);
                let declarator = ast::named_children(component)
                    .find(|c| c.kind() == "variable_declarator")
                    .and_then(|d| ast::field(d, "name"));
                (declarator, Type::Array(Box::new(element)))
            }
            _ => continue,
        };
        let Some(name_node) = name_node else {
            continue;
        };
        let component_name = name::name(ast::text(name_node, text));
        let origin = source_origin(file, component, Some(name_node));
        fields.push(FieldData {
            name: component_name.clone(),
            ty: ty.clone(),
            visibility: Visibility::Private,
            modifiers: Modifiers {
                is_final: true,
                ..Modifiers::default()
            },
            origin: origin.clone(),
            doc: None,
            constant_value: None,
        });
        if !methods
            .iter()
            .any(|m| m.name == component_name && m.params.is_empty())
        {
            methods.push(MethodData {
                name: component_name.clone(),
                type_params: Vec::new(),
                params: Vec::new(),
                ret: ty.clone(),
                throws: Vec::new(),
                visibility: Visibility::Public,
                modifiers: Modifiers::default(),
                origin: origin.clone(),
                doc: None,
                is_constructor: false,
            });
        }
        params.push(ParamData {
            name: component_name,
            ty,
        });
    }
    let canonical_declared = methods
        .iter()
        .any(|m| m.is_constructor && m.params.len() == params.len());
    if !canonical_declared {
        let record_name = ast::field(node, "name")
            .map(|n| name::name(ast::text(n, text)))
            .unwrap_or_else(|| name::name("record"));
        methods.push(MethodData {
            name: record_name,
            type_params: Vec::new(),
            params,
            ret: Type::VOID,
            throws: Vec::new(),
            visibility,
            modifiers: Modifiers {
                is_varargs,
                ..Modifiers::default()
            },
            origin: Origin::Synthetic,
            doc: None,
            is_constructor: true,
        });
    }
}

fn lower_field(
    node: Node<'_>,
    text: &str,
    scope: &Scope<'_>,
    file: crate::symbols::FileId,
    default_visibility: Visibility,
    out: &mut Vec<FieldData>,
) {
    let declared = ast::field(node, "type")
        .map(|ty| scope.lower_type(ty, text))
        .unwrap_or(Type::Unknown);
    let visibility = visibility_of(node, text, default_visibility);
    let modifiers = modifiers_of(node, text);
    let doc = javadoc::doc_for(node, text);
    for declarator in ast::named_children(node).filter(|c| c.kind() == "variable_declarator") {
        let Some(name_node) = ast::field(declarator, "name") else {
            continue;
        };
        // `int a[]` puts the extra dimensions on the declarator, not the type.
        let extra_dimensions = ast::named_children(declarator)
            .filter(|c| c.kind() == "dimensions")
            .map(|d| ast::text(d, text).matches('[').count())
            .sum::<usize>();
        let ty =
            (0..extra_dimensions).fold(declared.clone(), |inner, _| Type::Array(Box::new(inner)));
        out.push(FieldData {
            name: name::name(ast::text(name_node, text)),
            ty,
            visibility,
            modifiers,
            origin: source_origin(file, declarator, Some(name_node)),
            doc: doc.clone(),
            constant_value: ast::field(declarator, "value")
                .filter(|_| modifiers.is_final)
                .map(|value| Arc::from(ast::text(value, text))),
        });
    }
}

fn lower_method(
    node: Node<'_>,
    text: &str,
    outer: &Scope<'_>,
    file: crate::symbols::FileId,
    default_visibility: Visibility,
    class: &OutlineClass,
    is_constructor: bool,
) -> Option<MethodData> {
    let name_node = ast::field(node, "name")?;
    let mut scope = Scope {
        db: outer.db,
        outline: outer.outline,
        enclosing: outer.enclosing.clone(),
        type_vars: outer.type_vars.clone(),
    };
    let type_params = lower_type_params(node, text, &mut scope);

    let mut params = Vec::new();
    let mut is_varargs = false;
    if let Some(list) = ast::field(node, "parameters") {
        for parameter in ast::named_children(list) {
            match parameter.kind() {
                "formal_parameter" => {
                    let ty = ast::field(parameter, "type")
                        .map(|ty| scope.lower_type(ty, text))
                        .unwrap_or(Type::Unknown);
                    let name_node = ast::field(parameter, "name");
                    params.push(ParamData {
                        name: name_node
                            .map(|n| name::name(ast::text(n, text)))
                            .unwrap_or_else(|| name::name("arg")),
                        ty,
                    });
                }
                "spread_parameter" => {
                    is_varargs = true;
                    let element = ast::named_children(parameter)
                        .find(|c| c.kind() != "variable_declarator" && c.kind() != "modifiers")
                        .map(|ty| scope.lower_type(ty, text))
                        .unwrap_or(Type::Unknown);
                    let declarator = ast::named_children(parameter)
                        .find(|c| c.kind() == "variable_declarator")
                        .and_then(|d| ast::field(d, "name"));
                    params.push(ParamData {
                        name: declarator
                            .map(|n| name::name(ast::text(n, text)))
                            .unwrap_or_else(|| name::name("args")),
                        ty: Type::Array(Box::new(element)),
                    });
                }
                _ => {}
            }
        }
    }

    let ret = if is_constructor {
        Type::VOID
    } else {
        let declared = ast::field(node, "type")
            .map(|ty| scope.lower_type(ty, text))
            .unwrap_or(Type::Unknown);
        // `int f()[]` is legal and puts a dimension after the parameter list.
        let extra = ast::named_children(node)
            .filter(|c| c.kind() == "dimensions")
            .map(|d| ast::text(d, text).matches('[').count())
            .sum::<usize>();
        (0..extra).fold(declared, |inner, _| Type::Array(Box::new(inner)))
    };

    let throws = ast::named_children(node)
        .find(|c| c.kind() == "throws")
        .map(|list| {
            ast::named_children(list)
                .map(|ty| scope.lower_type(ty, text))
                .collect()
        })
        .unwrap_or_default();

    let mut modifiers = modifiers_of(node, text);
    modifiers.is_varargs = is_varargs;
    // An interface method without a body is abstract whether or not it says so.
    if matches!(class.kind, ClassKind::Interface | ClassKind::Annotation)
        && ast::field(node, "body").is_none()
    {
        modifiers.is_abstract = true;
    }

    Some(MethodData {
        name: name::name(ast::text(name_node, text)),
        type_params,
        params,
        ret,
        throws,
        visibility: visibility_of(node, text, default_visibility),
        modifiers,
        origin: source_origin(file, node, Some(name_node)),
        doc: javadoc::doc_for(node, text),
        is_constructor,
    })
}

/// Builds the type of `this` inside a class, with its own type parameters
/// applied so that members referring to them resolve.
pub fn self_type_of(class: &ClassData) -> Type {
    Type::Class {
        name: class.name.clone(),
        args: class
            .type_params
            .iter()
            .map(|p| {
                Type::Var(Box::new(TypeVar::new(
                    p.name.clone(),
                    Some(class.name.clone()),
                    p.bounds.clone(),
                )))
            })
            .collect(),
    }
}

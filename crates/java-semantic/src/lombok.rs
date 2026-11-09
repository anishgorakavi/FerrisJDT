//! Lombok: the members its annotation processor adds, modelled from source text.
//!
//! Lombok rewrites classes at compile time, so a language server that only reads the source sees
//! no `getName()`, no `builder()`, no `log`. This module reads the Lombok annotations on a lowered
//! class and adds the members they imply. It is deliberately conservative: a member Lombok would
//! skip because the class already declares one of that name is skipped here too, and nothing is
//! generated unless the file imports `lombok`.

use crate::name::{self, Name};
use crate::outline::{self, FileOutline, OutlineClass};
use crate::symbols::{ClassData, ClassKind, FieldData, MethodData, Modifiers, Origin, ParamData};
use crate::symbols::{TypeParam, Visibility};
use crate::types::{Primitive, Type, TypeVar, WildcardKind};
use java_syntax::{ast, Node};

/// A Lombok-relevant annotation: its simple name and raw argument text by key.
/// A lone unnamed argument is stored under `value`.
pub struct Ann {
    pub name: String,
    args: Vec<(String, String)>,
}

impl Ann {
    fn arg(&self, key: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.arg(key).map(|v| v == "true")
    }

    /// A string literal argument with its quotes removed.
    fn string(&self, key: &str) -> Option<String> {
        self.arg(key).map(|v| v.trim_matches('"').to_string())
    }

    /// `{"a", "b"}` or `"a"` as a list of strings.
    fn strings(&self, key: &str) -> Vec<String> {
        self.arg(key)
            .map(|v| {
                v.trim_matches(|c| c == '{' || c == '}')
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An `AccessLevel` argument: `None` for `NONE` (generate nothing), `Some(Public)` when the
    /// argument is absent.
    fn access(&self, key: &str) -> Option<Visibility> {
        match self.arg(key).map(|v| v.rsplit('.').next().unwrap_or("")) {
            Some("NONE") => None,
            Some("PROTECTED") => Some(Visibility::Protected),
            Some("PACKAGE") => Some(Visibility::Package),
            Some("PRIVATE") => Some(Visibility::Private),
            _ => Some(Visibility::Public),
        }
    }
}

/// Every annotation on a declaration, with its arguments.
pub fn annotations(node: Node<'_>, text: &str) -> Vec<Ann> {
    let Some(modifiers) = ast::named_children(node).find(|n| n.kind() == "modifiers") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for annotation in ast::named_children(modifiers)
        .filter(|m| matches!(m.kind(), "marker_annotation" | "annotation"))
    {
        let Some(name_node) = ast::field(annotation, "name") else {
            continue;
        };
        let dotted = outline::dotted(name_node, text);
        let mut args = Vec::new();
        if let Some(list) = ast::field(annotation, "arguments") {
            for argument in ast::named_children(list) {
                if argument.kind() == "element_value_pair" {
                    if let (Some(key), Some(value)) =
                        (ast::field(argument, "key"), ast::field(argument, "value"))
                    {
                        args.push((
                            ast::text(key, text).to_string(),
                            ast::text(value, text).to_string(),
                        ));
                    }
                } else {
                    args.push(("value".to_string(), ast::text(argument, text).to_string()));
                }
            }
        }
        found.push(Ann {
            name: name::simple_name(&dotted).to_string(),
            args,
        });
    }
    found
}

/// True when a file imports anything from Lombok, the gate for every rule here.
pub fn is_enabled(outline: &FileOutline) -> bool {
    outline
        .imports
        .iter()
        .any(|import| &*import.path == "lombok" || import.path.starts_with("lombok."))
}

/// The nested builder class a `@Builder` or `@SuperBuilder` declaration implies, if any.
pub fn builder_decl(node: Node<'_>, text: &str, simple: &str) -> Option<(String, bool)> {
    for ann in annotations(node, text) {
        match ann.name.as_str() {
            "Builder" => {
                let name = ann
                    .string("builderClassName")
                    .unwrap_or_else(|| format!("{simple}Builder"));
                return Some((name, false));
            }
            "SuperBuilder" => return Some((format!("{simple}Builder"), true)),
            _ => {}
        }
    }
    None
}

/// What one declared field contributes.
struct FieldInfo {
    name: String,
    ty: Type,
    origin: Origin,
    is_static: bool,
    is_final: bool,
    has_init: bool,
    anns: Vec<Ann>,
}

impl FieldInfo {
    fn ann(&self, name: &str) -> Option<&Ann> {
        self.anns.iter().find(|a| a.name == name)
    }

    fn non_null(&self) -> bool {
        self.anns
            .iter()
            .any(|a| matches!(a.name.as_str(), "NonNull" | "NotNull" | "Nonnull"))
    }
}

/// `@Accessors` settings, which change how getter and setter names are formed.
#[derive(Clone, Default)]
struct Accessors {
    fluent: bool,
    chain: bool,
    prefixes: Vec<String>,
}

impl Accessors {
    fn from(ann: Option<&Ann>) -> Accessors {
        let Some(ann) = ann else {
            return Accessors::default();
        };
        let fluent = ann.flag("fluent").unwrap_or(false);
        Accessors {
            fluent,
            chain: ann.flag("chain").unwrap_or(fluent),
            prefixes: ann.strings("prefix"),
        }
    }

    /// The field name with a configured prefix removed, when what follows is not lowercase.
    fn base(&self, field: &str) -> String {
        for prefix in &self.prefixes {
            if let Some(rest) = field.strip_prefix(prefix.as_str()) {
                if rest.chars().next().is_some_and(|c| !c.is_lowercase()) {
                    return rest.to_string();
                }
            }
        }
        field.to_string()
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn is_primitive_boolean(ty: &Type) -> bool {
    matches!(ty, Type::Primitive(Primitive::Boolean))
}

/// Strips a leading `is` from a boolean field name, as Lombok does: `isActive` is `active`.
fn without_is(base: &str) -> &str {
    match base.strip_prefix("is") {
        Some(rest) if rest.chars().next().is_some_and(|c| c.is_uppercase()) => rest,
        _ => base,
    }
}

fn getter_name(field: &FieldInfo, accessors: &Accessors) -> String {
    let base = accessors.base(&field.name);
    if accessors.fluent {
        return base;
    }
    if is_primitive_boolean(&field.ty) {
        return format!("is{}", capitalize(without_is(&base)));
    }
    format!("get{}", capitalize(&base))
}

fn setter_name(field: &FieldInfo, accessors: &Accessors, prefix: &str) -> String {
    let base = accessors.base(&field.name);
    if accessors.fluent {
        return base;
    }
    let base = if is_primitive_boolean(&field.ty) {
        without_is(&base).to_string()
    } else {
        base
    };
    format!("{prefix}{}", capitalize(&base))
}

/// The logger type each Lombok logging annotation declares.
fn logger_type(annotation: &str) -> Option<&'static str> {
    Some(match annotation {
        "Slf4j" => "org.slf4j.Logger",
        "XSlf4j" => "org.slf4j.ext.XLogger",
        "Log" => "java.util.logging.Logger",
        "Log4j" => "org.apache.log4j.Logger",
        "Log4j2" => "org.apache.logging.log4j.Logger",
        "CommonsLog" => "org.apache.commons.logging.Log",
        "JBossLog" => "org.jboss.logging.Logger",
        "Flogger" => "com.google.common.flogger.FluentLogger",
        _ => return None,
    })
}

/// Singular form of a `@Singular` field name, when Lombok can derive it.
fn singular_of(plural: &str) -> Option<String> {
    if let Some(stem) = plural.strip_suffix("ies") {
        return Some(format!("{stem}y"));
    }
    plural
        .strip_suffix('s')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn method(
    name: &str,
    params: Vec<(String, Type)>,
    ret: Type,
    visibility: Visibility,
    is_static: bool,
    origin: &Origin,
    note: &str,
) -> MethodData {
    MethodData {
        name: name::name(name),
        type_params: Vec::new(),
        params: params
            .into_iter()
            .map(|(name, ty)| ParamData {
                name: name::name(&name),
                ty,
            })
            .collect(),
        ret,
        throws: Vec::new(),
        visibility,
        modifiers: Modifiers {
            is_static,
            ..Modifiers::default()
        },
        origin: origin.clone(),
        doc: Some(format!("Generated by Lombok `{note}`.").into()),
        is_constructor: false,
    }
}

fn var(param: &TypeParam, owner: &Name) -> Type {
    Type::Var(Box::new(TypeVar::new(
        param.name.clone(),
        Some(owner.clone()),
        param.bounds.clone(),
    )))
}

fn wildcard(kind: WildcardKind, bound: Type) -> Type {
    Type::Wildcard {
        kind,
        bound: Some(Box::new(bound)),
    }
}

/// Adds the members Lombok generates for `data` and returns any classes it generates (builders).
pub fn augment(
    data: &mut ClassData,
    node: Node<'_>,
    text: &str,
    class: &OutlineClass,
    outline: &FileOutline,
    db: &crate::db::Db,
) -> Vec<ClassData> {
    if !is_enabled(outline)
        || class.is_anonymous
        || node.kind() != "class_declaration" && node.kind() != "enum_declaration"
    {
        return Vec::new();
    }
    let class_anns = annotations(node, text);
    let fields = field_infos(data, node, text);
    let has_field_anns = fields.iter().any(|f| !f.anns.is_empty());
    if class_anns.is_empty() && !has_field_anns {
        return Vec::new();
    }
    let class_ann = |name: &str| class_anns.iter().find(|a| a.name == name);
    let accessors = Accessors::from(class_ann("Accessors"));
    let is_data = class_ann("Data").is_some();
    let is_value = class_ann("Value").is_some();

    if let Some(defaults) = class_ann("FieldDefaults") {
        let level = defaults.access("level");
        let make_final = defaults.flag("makeFinal").unwrap_or(false);
        for field in data.fields.iter_mut().filter(|f| !f.modifiers.is_static) {
            if field.visibility == Visibility::Package && defaults.arg("level").is_some() {
                field.visibility = level.unwrap_or(Visibility::Package);
            }
            field.modifiers.is_final |= make_final;
        }
    }
    if is_value {
        for field in data.fields.iter_mut().filter(|f| !f.modifiers.is_static) {
            if field.visibility == Visibility::Package {
                field.visibility = Visibility::Private;
            }
            field.modifiers.is_final = true;
        }
    }

    // Lombok never replaces a method the class declares itself, compared by name alone.
    let declared: Vec<String> = data
        .methods
        .iter()
        .filter(|m| !m.is_constructor)
        .map(|m| m.name.to_lowercase())
        .collect();
    let mut added: Vec<MethodData> = Vec::new();
    let self_type = data.self_type();
    let push = |added: &mut Vec<MethodData>, method: MethodData| {
        let lowered = method.name.to_lowercase();
        if !declared.contains(&lowered) && !added.iter().any(|m| m.name.to_lowercase() == lowered) {
            added.push(method);
        }
    };

    for field in &fields {
        if field.name.starts_with('$') {
            continue;
        }
        let field_accessors = field
            .ann("Accessors")
            .map(|a| Accessors::from(Some(a)))
            .unwrap_or_else(|| accessors.clone());
        let getter = match field.ann("Getter") {
            Some(ann) => Some(ann.access("value")),
            None if (is_data || is_value || class_ann("Getter").is_some()) && !field.is_static => {
                Some(
                    class_ann("Getter")
                        .map(|a| a.access("value"))
                        .unwrap_or(Some(Visibility::Public)),
                )
            }
            None => None,
        };
        if let Some(Some(visibility)) = getter {
            push(
                &mut added,
                method(
                    &getter_name(field, &field_accessors),
                    Vec::new(),
                    field.ty.clone(),
                    visibility,
                    field.is_static,
                    &field.origin,
                    "@Getter",
                ),
            );
        }

        let setter = match field.ann("Setter") {
            Some(ann) => Some(ann.access("value")),
            None if (is_data || class_ann("Setter").is_some())
                && !field.is_static
                && !field.is_final
                && !is_value =>
            {
                Some(
                    class_ann("Setter")
                        .map(|a| a.access("value"))
                        .unwrap_or(Some(Visibility::Public)),
                )
            }
            None => None,
        };
        if let (Some(Some(visibility)), false) = (setter, field.is_final) {
            let base = field_accessors.base(&field.name);
            let ret = if field_accessors.chain {
                self_type.clone()
            } else {
                Type::VOID
            };
            push(
                &mut added,
                method(
                    &setter_name(field, &field_accessors, "set"),
                    vec![(base, field.ty.clone())],
                    ret,
                    visibility,
                    field.is_static,
                    &field.origin,
                    "@Setter",
                ),
            );
        }

        let with = match field.ann("With") {
            Some(ann) => Some(ann.access("value")),
            None => class_ann("With").map(|a| a.access("value")),
        };
        if let (Some(Some(visibility)), false) = (with, field.is_static) {
            let base = field_accessors.base(&field.name);
            let name = format!("with{}", capitalize(without_is_for(field, &base)));
            push(
                &mut added,
                method(
                    &name,
                    vec![(base, field.ty.clone())],
                    self_type.clone(),
                    visibility,
                    false,
                    &field.origin,
                    "@With",
                ),
            );
        }
    }

    for annotation in &class_anns {
        if let Some(logger) = logger_type(&annotation.name) {
            if data.find_field("log").is_none() {
                data.fields.push(FieldData {
                    name: name::name("log"),
                    ty: Type::class(logger),
                    visibility: Visibility::Private,
                    modifiers: Modifiers {
                        is_static: true,
                        is_final: true,
                        ..Modifiers::default()
                    },
                    origin: data.origin.clone(),
                    doc: Some(format!("Generated by Lombok `@{}`.", annotation.name).into()),
                    constant_value: None,
                });
            }
        }
    }

    let mut extra_classes = Vec::new();
    let builder_ann = class_ann("Builder").or_else(|| class_ann("SuperBuilder"));
    if let (Some(ann), Some(declared_builder)) = (builder_ann, class.builder_class.as_ref()) {
        extra_classes = builder_classes(
            data,
            &fields,
            ann,
            &declared_builder.name,
            declared_builder.is_super,
            db,
            &mut added,
        );
    }

    if class.kind == ClassKind::Class {
        let constructors = constructors_for(
            data,
            &class_anns,
            &fields,
            is_data,
            is_value,
            builder_ann.is_some_and(|a| a.name == "Builder"),
        );
        if !constructors.is_empty() {
            // The implicit no-argument constructor disappears once Lombok adds its own.
            data.methods
                .retain(|m| !(m.is_constructor && m.origin == Origin::Synthetic));
            data.methods.extend(constructors);
        }
    }

    data.methods.extend(added);
    for builder in &extra_classes {
        data.nested.push(builder.name.clone());
    }
    extra_classes
}

/// `@With` strips `is` from a boolean field the way setters do.
fn without_is_for<'a>(field: &FieldInfo, base: &'a str) -> &'a str {
    if is_primitive_boolean(&field.ty) {
        without_is(base)
    } else {
        base
    }
}

/// Reads each field declaration's annotations and initialiser, joined to the lowered field.
fn field_infos(data: &ClassData, node: Node<'_>, text: &str) -> Vec<FieldInfo> {
    let body = ast::field(node, "body");
    let Some(body) = body else {
        return Vec::new();
    };
    let members: Vec<Node<'_>> = ast::named_children(body)
        .flat_map(|member| {
            if member.kind() == "enum_body_declarations" {
                ast::named_children(member).collect::<Vec<_>>()
            } else {
                vec![member]
            }
        })
        .collect();
    let mut infos = Vec::new();
    for member in members
        .into_iter()
        .filter(|m| m.kind() == "field_declaration")
    {
        for declarator in ast::named_children(member).filter(|c| c.kind() == "variable_declarator")
        {
            let Some(name_node) = ast::field(declarator, "name") else {
                continue;
            };
            let field_name = ast::text(name_node, text);
            let Some(lowered) = data.find_field(field_name) else {
                continue;
            };
            infos.push(FieldInfo {
                name: field_name.to_string(),
                ty: lowered.ty.clone(),
                origin: lowered.origin.clone(),
                is_static: lowered.modifiers.is_static,
                is_final: lowered.modifiers.is_final,
                has_init: ast::field(declarator, "value").is_some(),
                anns: annotations(member, text),
            });
        }
    }
    infos
}

/// The constructors the class's annotations ask for.
fn constructors_for(
    data: &ClassData,
    class_anns: &[Ann],
    fields: &[FieldInfo],
    is_data: bool,
    is_value: bool,
    plain_builder: bool,
) -> Vec<MethodData> {
    let ann = |name: &str| class_anns.iter().find(|a| a.name == name);
    let explicit = data
        .methods
        .iter()
        .any(|m| m.is_constructor && m.origin != Origin::Synthetic);
    let instance: Vec<&FieldInfo> = fields
        .iter()
        .filter(|f| !f.is_static && !f.name.starts_with('$'))
        .collect();
    let all_args: Vec<&FieldInfo> = instance
        .iter()
        .copied()
        .filter(|f| !(f.is_final && f.has_init))
        .collect();
    let required: Vec<&FieldInfo> = instance
        .iter()
        .copied()
        .filter(|f| !f.has_init && (f.is_final || f.non_null()))
        .collect();

    let mut wanted: Vec<(&str, Vec<&FieldInfo>, Option<&Ann>)> = Vec::new();
    if let Some(a) = ann("NoArgsConstructor") {
        wanted.push(("@NoArgsConstructor", Vec::new(), Some(a)));
    }
    if let Some(a) = ann("RequiredArgsConstructor") {
        wanted.push(("@RequiredArgsConstructor", required.clone(), Some(a)));
    }
    if let Some(a) = ann("AllArgsConstructor") {
        wanted.push(("@AllArgsConstructor", all_args.clone(), Some(a)));
    }
    if wanted.is_empty() && !explicit {
        if is_value {
            wanted.push(("@Value", all_args.clone(), None));
        } else if is_data {
            wanted.push(("@Data", required.clone(), None));
        } else if plain_builder || ann("SuperBuilder").is_some() {
            let visibility = Visibility::Package;
            let params = all_args.clone();
            return vec![constructor(data, &params, visibility, "@Builder")];
        }
    }

    let mut out = Vec::new();
    for (note, params, config) in wanted {
        let visibility = config
            .map(|a| a.access("access"))
            .unwrap_or(Some(Visibility::Public));
        let Some(visibility) = visibility else {
            continue;
        };
        let static_name = config.and_then(|a| a.string("staticName"));
        match static_name {
            Some(factory) => {
                out.push(constructor(data, &params, Visibility::Private, note));
                let mut generated = method(
                    &factory,
                    params
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                    data.self_type(),
                    visibility,
                    true,
                    &data.origin,
                    note,
                );
                generated.type_params = data.type_params.clone();
                out.push(generated);
            }
            None => out.push(constructor(data, &params, visibility, note)),
        }
    }
    out
}

fn constructor(
    data: &ClassData,
    params: &[&FieldInfo],
    visibility: Visibility,
    note: &str,
) -> MethodData {
    let mut generated = method(
        data.simple_name(),
        params
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect(),
        Type::VOID,
        visibility,
        false,
        &data.origin,
        note,
    );
    generated.is_constructor = true;
    generated
}

/// Builds the nested builder class (and its `Impl` for `@SuperBuilder`) and the entry points on
/// the annotated class.
fn builder_classes(
    data: &ClassData,
    fields: &[FieldInfo],
    ann: &Ann,
    builder_simple: &str,
    is_super: bool,
    db: &crate::db::Db,
    added: &mut Vec<MethodData>,
) -> Vec<ClassData> {
    let builder_binary = name::name(&format!("{}${}", data.name, builder_simple));
    let class_vars: Vec<Type> = data
        .type_params
        .iter()
        .map(|p| var(p, &data.name))
        .collect();
    let prefix = ann.string("setterPrefix").unwrap_or_default();
    let build_name = ann
        .string("buildMethodName")
        .unwrap_or_else(|| "build".into());
    let entry_name = ann
        .string("builderMethodName")
        .unwrap_or_else(|| "builder".into());

    let mut builder_params = data.type_params.clone();
    let (c_var, b_var);
    if is_super {
        let c = TypeParam {
            name: name::name("C"),
            bounds: vec![data.self_type()],
        };
        let b = TypeParam {
            name: name::name("B"),
            bounds: Vec::new(),
        };
        c_var = Some(var(&c, &builder_binary));
        b_var = Some(var(&b, &builder_binary));
        builder_params.push(c);
        builder_params.push(b);
    } else {
        c_var = None;
        b_var = None;
    }

    let mut builder = ClassData {
        name: builder_binary.clone(),
        kind: ClassKind::Class,
        visibility: Visibility::Public,
        modifiers: Modifiers {
            is_static: true,
            is_abstract: is_super,
            ..Modifiers::default()
        },
        type_params: builder_params,
        superclass: Some(Type::object()),
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        nested: Vec::new(),
        origin: data.origin.clone(),
        doc: Some("Generated by Lombok `@Builder`.".into()),
    };
    let own_type = builder.self_type();
    let chain_type = if is_super {
        b_var.clone().unwrap_or_else(|| own_type.clone())
    } else {
        own_type.clone()
    };

    if is_super {
        // The parent's builder, when the superclass is itself a `@SuperBuilder`.
        if let Some(Type::Class { name: parent, args }) = &data.superclass {
            let parent_simple = name::simple_name(parent).to_string();
            let parent_builder = format!("{parent}${parent_simple}Builder");
            let has_parent_builder = db.class(parent).is_some_and(|p| {
                p.nested
                    .iter()
                    .any(|n| **n == *format!("{parent_builder}Impl"))
            });
            if has_parent_builder {
                let mut super_args = args.clone();
                super_args.extend(c_var.clone());
                super_args.extend(b_var.clone());
                builder.superclass = Some(Type::generic(parent_builder, super_args));
            }
        }
    }

    for field in fields {
        if field.is_static || field.name.starts_with('$') || (field.is_final && field.has_init) {
            continue;
        }
        if let (Some(singular), true) = (field.ann("Singular"), is_collection(&field.ty)) {
            singular_methods(&mut builder, field, singular, &prefix, &chain_type);
            continue;
        }
        let name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{prefix}{}", capitalize(&field.name))
        };
        builder.methods.push(method(
            &name,
            vec![(field.name.clone(), field.ty.clone())],
            chain_type.clone(),
            Visibility::Public,
            false,
            &field.origin,
            "@Builder",
        ));
    }

    let build_return = c_var.clone().unwrap_or_else(|| data.self_type());
    let mut build = method(
        &build_name,
        Vec::new(),
        build_return,
        Visibility::Public,
        false,
        &data.origin,
        "@Builder",
    );
    build.modifiers.is_abstract = is_super;
    builder.methods.push(build);
    builder.methods.push(method(
        "toString",
        Vec::new(),
        Type::string(),
        Visibility::Public,
        false,
        &data.origin,
        "@Builder",
    ));

    let mut classes = Vec::new();
    let entry_return;
    if is_super {
        let impl_binary = name::name(&format!("{builder_binary}Impl"));
        let impl_params = data.type_params.clone();
        let impl_self = Type::Class {
            name: impl_binary.clone(),
            args: class_vars.clone(),
        };
        let mut super_args = class_vars.clone();
        super_args.push(data.self_type());
        super_args.push(impl_self.clone());
        classes.push(ClassData {
            name: impl_binary,
            kind: ClassKind::Class,
            visibility: Visibility::Private,
            modifiers: Modifiers {
                is_static: true,
                is_final: true,
                ..Modifiers::default()
            },
            type_params: impl_params,
            superclass: Some(Type::generic(builder_binary.clone(), super_args.clone())),
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            nested: Vec::new(),
            origin: data.origin.clone(),
            doc: Some("Generated by Lombok `@SuperBuilder`.".into()),
        });
        entry_return = Type::generic(builder_binary.clone(), super_args);
    } else {
        entry_return = builder.self_type();
        if ann.flag("toBuilder") == Some(true) {
            added.push(method(
                "toBuilder",
                Vec::new(),
                entry_return.clone(),
                Visibility::Public,
                false,
                &data.origin,
                "@Builder(toBuilder = true)",
            ));
        }
    }
    let mut entry = method(
        &entry_name,
        Vec::new(),
        entry_return,
        Visibility::Public,
        true,
        &data.origin,
        if is_super {
            "@SuperBuilder"
        } else {
            "@Builder"
        },
    );
    entry.type_params = data.type_params.clone();
    added.push(entry);
    classes.insert(0, builder);
    classes
}

fn is_collection(ty: &Type) -> bool {
    matches!(
        ty.class_name().map(|n| &**n),
        Some(
            "java.util.List"
                | "java.util.Set"
                | "java.util.Collection"
                | "java.util.SortedSet"
                | "java.util.Map"
                | "java.util.SortedMap"
        )
    )
}

/// `@Singular`: one-element adder, bulk adder, and clear, in place of a plain setter.
fn singular_methods(
    builder: &mut ClassData,
    field: &FieldInfo,
    ann: &Ann,
    prefix: &str,
    chain: &Type,
) {
    let singular = ann
        .string("value")
        .or_else(|| singular_of(&field.name))
        .unwrap_or_else(|| field.name.clone());
    let decorate = |name: &str| {
        if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}{}", capitalize(name))
        }
    };
    let args = field.ty.type_args().to_vec();
    let is_map = field
        .ty
        .class_name()
        .is_some_and(|n| n.ends_with("Map") || n.ends_with("SortedMap"));
    let (one, many): (Vec<(String, Type)>, Type) = if is_map && args.len() == 2 {
        (
            vec![
                ("key".into(), args[0].clone()),
                ("value".into(), args[1].clone()),
            ],
            Type::generic(
                "java.util.Map",
                vec![
                    wildcard(WildcardKind::Extends, args[0].clone()),
                    wildcard(WildcardKind::Extends, args[1].clone()),
                ],
            ),
        )
    } else if let Some(element) = args.first() {
        (
            vec![(singular.clone(), element.clone())],
            Type::generic(
                "java.util.Collection",
                vec![wildcard(WildcardKind::Extends, element.clone())],
            ),
        )
    } else {
        return;
    };
    let origin = field.origin.clone();
    builder.methods.push(method(
        &decorate(&singular),
        one,
        chain.clone(),
        Visibility::Public,
        false,
        &origin,
        "@Singular",
    ));
    builder.methods.push(method(
        &decorate(&field.name),
        vec![(field.name.clone(), many)],
        chain.clone(),
        Visibility::Public,
        false,
        &origin,
        "@Singular",
    ));
    builder.methods.push(method(
        &format!("clear{}", capitalize(&field.name)),
        Vec::new(),
        chain.clone(),
        Visibility::Public,
        false,
        &origin,
        "@Singular",
    ));
}

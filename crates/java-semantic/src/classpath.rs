//! The classpath: the JDK standard library and any extra JARs.
//!
//! Archive directories are read once at startup, which costs about a megabyte
//! for a 20k-entry `rt.jar`; individual classes are decoded only when a
//! request actually needs them, and the decoded form is cached. Nothing here
//! starts a JVM.

use crate::name::{self, Name};
use crate::symbols::{
    ClassData, ClassKind, ClassRef, FieldData, MethodData, Modifiers, Origin, ParamData, TypeParam,
    Visibility,
};
use crate::types::{Type, TypeVar, WildcardKind};
use java_classfile::{
    class, flags, signature::SigArg, ClassInfo, JImage, Jar, SigType, TypeParamSig,
};
use project_model::{Project, StdlibSource};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Upper bound on decoded classes held at once. A session touches a few
/// hundred; the cap exists so a scripted client cannot grow the cache without
/// limit. On overflow the cache is dropped wholesale, which costs a re-decode
/// rather than risking stale entries.
const MAX_CACHED_CLASSES: usize = 8192;

/// Where an archive's class bytes come from.
enum Store {
    /// A ZIP. `suffix` is the entry extension: `.class` in a JAR, `.sig` in `ct.sym`.
    Jar { jar: Jar, suffix: &'static str },
    /// The JDK's own `lib/modules`; each prefix is a module name.
    Image(JImage),
}

struct Archive {
    store: Store,
    /// Interned path prefixes. A JAR has the single empty prefix; `ct.sym`
    /// entries are prefixed with a release and module directory; a jimage
    /// archive has one prefix per module.
    prefixes: Vec<Box<str>>,
    display: Arc<str>,
}

/// One class available on the classpath, before it has been decoded.
struct ClassEntry {
    binary: Box<str>,
    archive: u16,
    prefix: u16,
}

pub struct Classpath {
    /// The JDK's `src.zip`, when it has one: the source of parameter names
    /// and documentation that class files do not carry.
    sources: Option<Jar>,
    archives: Vec<Archive>,
    /// Sorted by binary name, so lookup is a binary search.
    classes: Vec<ClassEntry>,
    /// Indices into `classes`, sorted by simple name. Avoids storing a second
    /// copy of every name.
    by_simple: Vec<u32>,
    cache: RwLock<FxHashMap<Box<str>, Option<ClassRef>>>,
}

impl Classpath {
    pub fn empty() -> Classpath {
        Classpath {
            sources: None,
            archives: Vec::new(),
            classes: Vec::new(),
            by_simple: Vec::new(),
            cache: RwLock::new(FxHashMap::default()),
        }
    }

    /// Indexes the project's JDK standard library and extra JARs.
    pub fn load(project: &Project) -> Classpath {
        let mut classpath = Classpath::empty();
        if let Some(jdk) = &project.jdk {
            match &jdk.stdlib {
                StdlibSource::RtJar(path) => classpath.add_jar(path),
                StdlibSource::CtSym { path, release } => {
                    // `ct.sym` omits the JDK's own release; that one is read from `lib/modules`.
                    if classpath.add_ct_sym(path, *release) {
                        classpath.add_jimage(&path.with_file_name("modules"));
                    }
                }
            }
            if let Some(sources) = &jdk.sources {
                match Jar::open(sources) {
                    Ok(jar) => {
                        tracing::debug!(path = %sources.display(), entries = jar.len(), "attached jdk sources");
                        classpath.sources = Some(jar);
                    }
                    Err(error) => {
                        tracing::warn!(path = %sources.display(), %error, "ignoring unreadable src.zip")
                    }
                }
            }
        }
        for jar in &project.classpath {
            classpath.add_jar(jar);
        }
        classpath.finish();
        tracing::info!(
            classes = classpath.classes.len(),
            archives = classpath.archives.len(),
            "indexed classpath"
        );
        classpath
    }

    /// Indexes an ordinary JAR of `.class` entries.
    pub fn add_jar(&mut self, path: &Path) {
        let jar = match Jar::open(path) {
            Ok(jar) => jar,
            Err(error) => {
                // An unreadable JAR degrades completion for its classes but
                // must never stop the server from starting.
                tracing::warn!(path = %path.display(), %error, "skipping unreadable archive");
                return;
            }
        };
        let archive_index = self.archives.len() as u16;
        let mut added = 0usize;
        for entry in jar.entry_names() {
            let Some(internal) = entry.strip_suffix(".class") else {
                continue;
            };
            if internal.ends_with("module-info") || internal.ends_with("package-info") {
                continue;
            }
            self.classes.push(ClassEntry {
                binary: internal.replace('/', ".").into_boxed_str(),
                archive: archive_index,
                prefix: 0,
            });
            added += 1;
        }
        tracing::debug!(path = %path.display(), classes = added, "indexed archive");
        self.archives.push(Archive {
            store: Store::Jar {
                jar,
                suffix: ".class",
            },
            prefixes: vec![Box::from("")],
            display: Arc::from(path.to_string_lossy().as_ref()),
        });
    }

    /// Indexes a JDK 9+ `ct.sym`, keeping only the requested release.
    ///
    /// Entries are `<releases>/<module>/<package>/<Class>.sig`, where
    /// `<releases>` lists every release the signature applies to as base-36
    /// characters, so release 8 is any directory whose first segment contains
    /// `8`. A later release's directory may also contain the class, so the
    /// first match for a given name wins.
    ///
    /// `ct.sym` omits the JDK's own release, which lives in the `modules` image
    /// instead, so a request for that release or a newer one reads the newest
    /// release the archive does hold. Returns true in that case, when the caller should
    /// overlay the image with `add_jimage`.
    pub fn add_ct_sym(&mut self, path: &Path, release: u32) -> bool {
        let jar = match Jar::open(path) {
            Ok(jar) => jar,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "skipping unreadable ct.sym");
                return false;
            }
        };
        let newest = jar
            .entry_names()
            .filter(|entry| entry.ends_with(".sig"))
            .filter_map(|entry| entry.split('/').next())
            .flat_map(str::chars)
            .filter_map(|c| c.to_digit(36))
            .max()
            .unwrap_or(release);
        let wanted = project_model::jdk::ct_sym_release_char(release.min(newest));
        let archive_index = self.archives.len() as u16;
        let mut prefixes: Vec<Box<str>> = Vec::new();
        let mut added = 0usize;
        for entry in jar.entry_names() {
            let Some(body) = entry.strip_suffix(".sig") else {
                continue;
            };
            let mut segments = body.splitn(3, '/');
            let (Some(releases), Some(module), Some(internal)) =
                (segments.next(), segments.next(), segments.next())
            else {
                continue;
            };
            if !releases.contains(wanted) {
                continue;
            }
            if internal.ends_with("module-info") || internal.ends_with("package-info") {
                continue;
            }
            let prefix = format!("{releases}/{module}/");
            let prefix_index = match prefixes.iter().position(|p| **p == *prefix) {
                Some(index) => index,
                None => {
                    prefixes.push(prefix.into_boxed_str());
                    prefixes.len() - 1
                }
            };
            self.classes.push(ClassEntry {
                binary: internal.replace('/', ".").into_boxed_str(),
                archive: archive_index,
                prefix: prefix_index as u16,
            });
            added += 1;
        }
        tracing::debug!(path = %path.display(), classes = added, release, "indexed ct.sym");
        self.archives.push(Archive {
            store: Store::Jar {
                jar,
                suffix: ".sig",
            },
            prefixes,
            display: Arc::from(path.to_string_lossy().as_ref()),
        });
        release > newest
    }

    /// Overlays the JDK's `lib/modules` on the `ct.sym` classes already indexed.
    ///
    /// `ct.sym` supplies which classes are public API; the image supplies their current
    /// bodies, so members added in the JDK's own release (`List.getFirst`) are visible, and it
    /// supplies public classes new in that release (`SequencedCollection`). Packages that
    /// `ct.sym` does not list are internal and are never indexed.
    pub fn add_jimage(&mut self, path: &Path) {
        let image = match JImage::open(path) {
            Ok(image) => image,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring unreadable jdk modules image");
                return;
            }
        };
        let known: rustc_hash::FxHashSet<&str> =
            self.classes.iter().map(|entry| &*entry.binary).collect();
        let packages: rustc_hash::FxHashSet<&str> = known
            .iter()
            .map(|binary| name::package_of(binary))
            .collect();
        let archive_index = self.archives.len() as u16;
        let mut modules: Vec<Box<str>> = Vec::new();
        let mut entries: Vec<ClassEntry> = Vec::new();
        let (mut replaced, mut added) = (0usize, 0usize);
        image.for_each_class(|module, internal| {
            let binary = internal.replace('/', ".");
            if !packages.contains(name::package_of(&binary)) {
                return;
            }
            if known.contains(binary.as_str()) {
                replaced += 1;
            } else {
                // A class new in this release: index it only if it is public API.
                let simple = name::simple_name(&binary);
                if simple.rsplit('$').next().is_some_and(|last| last.starts_with(|c: char| c.is_ascii_digit())) {
                    return;
                }
                let public = matches!(
                    image.read_class(module, internal),
                    Ok(Some(bytes)) if class::parse(&bytes).is_ok_and(|info| info.access & flags::PUBLIC != 0)
                );
                if !public {
                    return;
                }
                added += 1;
            }
            let prefix = match modules.iter().position(|m| **m == *module) {
                Some(index) => index,
                None => {
                    modules.push(Box::from(module));
                    modules.len() - 1
                }
            };
            entries.push(ClassEntry {
                binary: binary.into_boxed_str(),
                archive: archive_index,
                prefix: prefix as u16,
            });
        });
        tracing::debug!(path = %path.display(), replaced, added, "overlaid jdk modules image");
        // Earlier entries win when the index is deduplicated, so the image goes first.
        self.classes.splice(0..0, entries);
        self.archives.push(Archive {
            store: Store::Image(image),
            prefixes: modules,
            display: Arc::from(path.to_string_lossy().as_ref()),
        });
    }

    /// Sorts and deduplicates the index. Must be called after the last archive.
    pub fn finish(&mut self) {
        self.classes.sort_by(|a, b| a.binary.cmp(&b.binary));
        // Earlier archives win, matching classpath precedence.
        self.classes.dedup_by(|a, b| a.binary == b.binary);
        self.classes.shrink_to_fit();
        self.by_simple = (0..self.classes.len() as u32).collect();
        self.by_simple.sort_by(|&a, &b| {
            let left = &self.classes[a as usize].binary;
            let right = &self.classes[b as usize].binary;
            name::simple_name(left)
                .cmp(name::simple_name(right))
                .then_with(|| left.cmp(right))
        });
    }

    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    pub fn contains(&self, binary: &str) -> bool {
        self.index_of(binary).is_some()
    }

    fn index_of(&self, binary: &str) -> Option<usize> {
        self.classes
            .binary_search_by(|entry| (*entry.binary).cmp(binary))
            .ok()
    }

    /// Binary names of every classpath class with the given simple name.
    pub fn classes_named(&self, simple: &str) -> Vec<&str> {
        let start = self
            .by_simple
            .partition_point(|&i| name::simple_name(&self.classes[i as usize].binary) < simple);
        self.by_simple[start..]
            .iter()
            .map(|&i| &*self.classes[i as usize].binary)
            .take_while(|binary| name::simple_name(binary) == simple)
            .collect()
    }

    /// Binary names declared directly in a package, excluding subpackages.
    pub fn classes_in_package<'a>(&'a self, package: &str) -> Vec<&'a str> {
        let skip = package.len() + usize::from(!package.is_empty());
        self.names_with_prefix(package)
            .filter(move |binary| !binary[skip.min(binary.len())..].contains('.'))
            .collect()
    }

    /// Immediate subpackage names of a package.
    pub fn subpackages<'a>(&'a self, package: &str) -> Vec<&'a str> {
        let skip = package.len() + usize::from(!package.is_empty());
        let mut names: Vec<&str> = self
            .names_with_prefix(package)
            .filter_map(move |binary| {
                let rest = &binary[skip..];
                let head = rest.split('.').next()?;
                // A package component starts lowercase by convention; more
                // reliably, a class name is the final component.
                (rest.len() > head.len()).then_some(&binary[skip..skip + head.len()])
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    fn names_with_prefix<'a>(&'a self, package: &str) -> impl Iterator<Item = &'a str> {
        let prefix = if package.is_empty() {
            String::new()
        } else {
            format!("{package}.")
        };
        let start = self.classes.partition_point(|e| *e.binary < *prefix);
        self.classes[start..]
            .iter()
            .map(|e| &*e.binary)
            .take_while(move |binary: &&str| binary.starts_with(prefix.as_str()))
    }

    /// All indexed binary names, for workspace symbol search.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        self.classes.iter().map(|e| &*e.binary)
    }

    /// Decodes a class, caching the result. Returns `None` when the class is
    /// absent or fails to decode; a failure is cached so a corrupt entry is
    /// not re-read on every keystroke.
    pub fn class(&self, binary: &str) -> Option<ClassRef> {
        if let Ok(cache) = self.cache.read() {
            if let Some(cached) = cache.get(binary) {
                return cached.clone();
            }
        }
        let decoded = self.decode(binary);
        if let Ok(mut cache) = self.cache.write() {
            if cache.len() >= MAX_CACHED_CLASSES {
                tracing::debug!("classpath cache full; clearing");
                cache.clear();
            }
            cache.insert(Box::from(binary), decoded.clone());
        }
        decoded
    }

    fn decode(&self, binary: &str) -> Option<ClassRef> {
        let entry = self.classes.get(self.index_of(binary)?)?;
        let archive = self.archives.get(entry.archive as usize)?;
        let prefix = archive.prefixes.get(entry.prefix as usize)?;
        let internal = entry.binary.replace('.', "/");
        let read = match &archive.store {
            Store::Jar { jar, suffix } => jar.read(&format!("{prefix}{internal}{suffix}")),
            Store::Image(image) => image.read_class(prefix, &internal),
        };
        let bytes = match read {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(class = binary, %error, "failed to read class entry");
                return None;
            }
        };
        match class::parse(&bytes) {
            Ok(info) => {
                let mut lowered = lower_class_file(&info, &archive.display);
                // Parameter names and javadoc come from the JDK's own sources,
                // which is the only place they exist.
                if let Some(sources) = &self.sources {
                    if let Some(attachment) = crate::attach::attachment_for(sources, binary) {
                        crate::attach::apply(&mut lowered, &attachment);
                    }
                }
                Some(Arc::new(lowered))
            }
            Err(error) => {
                tracing::warn!(class = binary, %error, "failed to decode class file");
                None
            }
        }
    }

    /// Number of decoded classes currently cached, for diagnostics.
    pub fn cached_classes(&self) -> usize {
        self.cache.read().map(|c| c.len()).unwrap_or(0)
    }
}

fn visibility_of(access: u16) -> Visibility {
    if access & flags::PUBLIC != 0 {
        Visibility::Public
    } else if access & flags::PROTECTED != 0 {
        Visibility::Protected
    } else if access & flags::PRIVATE != 0 {
        Visibility::Private
    } else {
        Visibility::Package
    }
}

fn modifiers_of(access: u16, deprecated: bool, is_interface_member: bool) -> Modifiers {
    Modifiers {
        is_static: access & flags::STATIC != 0,
        is_final: access & flags::FINAL != 0,
        is_abstract: access & flags::ABSTRACT != 0,
        is_synchronized: access & flags::SYNCHRONIZED != 0,
        is_native: access & flags::NATIVE != 0,
        is_transient: access & flags::TRANSIENT != 0,
        is_volatile: access & flags::VOLATILE != 0,
        // A concrete, non-static interface method is a default method.
        is_default: is_interface_member
            && access & flags::ABSTRACT == 0
            && access & flags::STATIC == 0,
        is_varargs: access & flags::VARARGS != 0,
        is_deprecated: deprecated,
        is_synthetic: access & (flags::SYNTHETIC | flags::BRIDGE) != 0,
    }
}

/// Context for converting signature types: which names are type variables, and
/// what their bounds are.
struct VarScope<'a> {
    owner: &'a Name,
    /// Bounds are stored already converted, with inner variables unbounded.
    vars: Vec<(Name, Vec<Type>)>,
}

impl VarScope<'_> {
    fn lookup(&self, name: &str) -> Option<&(Name, Vec<Type>)> {
        self.vars.iter().rev().find(|(n, _)| &**n == name)
    }
}

fn convert(sig: &SigType, scope: &VarScope<'_>) -> Type {
    match sig {
        SigType::Primitive(c) => match c {
            'B' => Type::Primitive(crate::types::Primitive::Byte),
            'C' => Type::Primitive(crate::types::Primitive::Char),
            'D' => Type::Primitive(crate::types::Primitive::Double),
            'F' => Type::Primitive(crate::types::Primitive::Float),
            'I' => Type::Primitive(crate::types::Primitive::Int),
            'J' => Type::Primitive(crate::types::Primitive::Long),
            'S' => Type::Primitive(crate::types::Primitive::Short),
            'Z' => Type::Primitive(crate::types::Primitive::Boolean),
            _ => Type::Primitive(crate::types::Primitive::Void),
        },
        SigType::Array(inner) => Type::Array(Box::new(convert(inner, scope))),
        SigType::Var(var) => {
            let bounds = scope
                .lookup(var)
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            Type::Var(Box::new(TypeVar::new(
                name::name(var),
                Some(scope.owner.clone()),
                bounds,
            )))
        }
        SigType::Class {
            name: internal,
            args,
            ..
        } => Type::Class {
            name: name::name(&internal.replace('/', ".")),
            args: args.iter().map(|a| convert_arg(a, scope)).collect(),
        },
    }
}

fn convert_arg(arg: &SigArg, scope: &VarScope<'_>) -> Type {
    match arg {
        SigArg::Exact(ty) => convert(ty, scope),
        SigArg::Extends(ty) => Type::Wildcard {
            kind: WildcardKind::Extends,
            bound: Some(Box::new(convert(ty, scope))),
        },
        SigArg::Super(ty) => Type::Wildcard {
            kind: WildcardKind::Super,
            bound: Some(Box::new(convert(ty, scope))),
        },
        SigArg::Any => Type::Wildcard {
            kind: WildcardKind::Unbounded,
            bound: None,
        },
    }
}

/// Converts type parameter declarations, then records their bounds in `scope`.
///
/// Bounds are converted against the scope as it grows, so an earlier parameter
/// can appear in a later one's bound; the variables inside a bound are left
/// unbounded, which keeps `E extends Comparable<? super E>` finite.
fn convert_type_params(sigs: &[TypeParamSig], scope: &mut VarScope<'_>) -> Vec<TypeParam> {
    // Register the names first so a bound may refer to any parameter.
    let first_new = scope.vars.len();
    for sig in sigs {
        scope.vars.push((name::name(&sig.name), Vec::new()));
    }
    let mut params = Vec::with_capacity(sigs.len());
    for (offset, sig) in sigs.iter().enumerate() {
        let bounds: Vec<Type> = sig.bounds.iter().map(|b| convert(b, scope)).collect();
        scope.vars[first_new + offset].1 = bounds.clone();
        params.push(TypeParam {
            name: name::name(&sig.name),
            bounds,
        });
    }
    params
}

/// Lowers a decoded class file into the resolver's own representation.
pub fn lower_class_file(info: &ClassInfo, archive: &Arc<str>) -> ClassData {
    let binary = name::name(&info.internal_name.replace('/', "."));
    let origin = Origin::Classpath {
        archive: archive.clone(),
    };
    let mut scope = VarScope {
        owner: &binary,
        vars: Vec::new(),
    };
    let type_params = convert_type_params(&info.type_params, &mut scope);

    let kind = if info.is_annotation() {
        ClassKind::Annotation
    } else if info.is_interface() {
        ClassKind::Interface
    } else if info.is_enum() {
        ClassKind::Enum
    } else {
        ClassKind::Class
    };
    let is_interface = kind == ClassKind::Interface;

    let superclass = info.superclass.as_ref().map(|s| convert(s, &scope));
    let interfaces = info.interfaces.iter().map(|i| convert(i, &scope)).collect();

    let fields = info
        .fields
        .iter()
        .map(|field| FieldData {
            name: name::name(&field.name),
            ty: convert(&field.ty, &scope),
            visibility: visibility_of(field.access),
            modifiers: modifiers_of(field.access, field.deprecated, is_interface),
            origin: origin.clone(),
            doc: None,
            constant_value: None,
        })
        .collect();

    let methods = info
        .methods
        .iter()
        .map(|method| {
            let mut method_scope = VarScope {
                owner: &binary,
                vars: scope.vars.clone(),
            };
            let method_type_params = convert_type_params(&method.type_params, &mut method_scope);
            let is_constructor = method.name == "<init>";
            let params = method
                .params
                .iter()
                .enumerate()
                .map(|(index, ty)| ParamData {
                    // Class files carry parameter names only when compiled
                    // with `-parameters`, which the JDK is not; fall back to
                    // the positional names javadoc uses.
                    name: method
                        .param_names
                        .as_ref()
                        .and_then(|names| names.get(index))
                        .filter(|name| !name.is_empty())
                        .map(|name| name::name(name))
                        .unwrap_or_else(|| name::name(&format!("arg{index}"))),
                    ty: convert(ty, &method_scope),
                })
                .collect();
            MethodData {
                name: if is_constructor {
                    name::name(name::simple_name(&binary))
                } else {
                    name::name(&method.name)
                },
                type_params: method_type_params,
                params,
                ret: convert(&method.ret, &method_scope),
                throws: method
                    .throws
                    .iter()
                    .map(|t| convert(t, &method_scope))
                    .collect(),
                visibility: visibility_of(method.access),
                modifiers: modifiers_of(method.access, method.deprecated, is_interface),
                origin: origin.clone(),
                doc: None,
                is_constructor,
            }
        })
        // `<clinit>` is the static initialiser and is not a callable member.
        .filter(|method| &*method.name != "<clinit>")
        .collect();

    let nested = info
        .inner_classes
        .iter()
        .filter(|inner| inner.outer.as_deref() == Some(info.internal_name.as_str()))
        .map(|inner| name::name(&inner.inner.replace('/', ".")))
        .collect();

    ClassData {
        name: binary,
        kind,
        visibility: visibility_of(info.access),
        modifiers: modifiers_of(info.access, info.deprecated, false),
        type_params,
        superclass,
        interfaces,
        fields,
        methods,
        nested,
        origin,
        doc: None,
    }
}

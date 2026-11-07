//! Class file decoding: access flags, fields, methods, and attributes.

use crate::constant_pool::ConstantPool;
use crate::error::{ClassFileError, Result};
use crate::reader::Reader;
use crate::signature::{self, SigType, TypeParamSig};

pub mod flags {
    pub const PUBLIC: u16 = 0x0001;
    pub const PRIVATE: u16 = 0x0002;
    pub const PROTECTED: u16 = 0x0004;
    pub const STATIC: u16 = 0x0008;
    pub const FINAL: u16 = 0x0010;
    pub const SUPER: u16 = 0x0020;
    pub const SYNCHRONIZED: u16 = 0x0020;
    pub const VOLATILE: u16 = 0x0040;
    pub const BRIDGE: u16 = 0x0040;
    pub const TRANSIENT: u16 = 0x0080;
    pub const VARARGS: u16 = 0x0080;
    pub const NATIVE: u16 = 0x0100;
    pub const INTERFACE: u16 = 0x0200;
    pub const ABSTRACT: u16 = 0x0400;
    pub const SYNTHETIC: u16 = 0x1000;
    pub const ANNOTATION: u16 = 0x2000;
    pub const ENUM: u16 = 0x4000;
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub access: u16,
    pub ty: SigType,
    pub deprecated: bool,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub access: u16,
    pub type_params: Vec<TypeParamSig>,
    pub params: Vec<SigType>,
    pub ret: SigType,
    /// Declared checked exceptions, from the `Exceptions` attribute.
    pub throws: Vec<SigType>,
    /// Present only when the class was compiled with `-parameters`.
    pub param_names: Option<Vec<String>>,
    pub deprecated: bool,
}

#[derive(Debug, Clone)]
pub struct InnerClassInfo {
    pub inner: String,
    pub outer: Option<String>,
    pub simple_name: Option<String>,
    pub access: u16,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Internal name with `/` package separators and `$` for nesting.
    pub internal_name: String,
    pub access: u16,
    pub type_params: Vec<TypeParamSig>,
    pub superclass: Option<SigType>,
    pub interfaces: Vec<SigType>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub inner_classes: Vec<InnerClassInfo>,
    pub deprecated: bool,
    pub source_file: Option<String>,
    pub major_version: u16,
}

impl ClassInfo {
    pub fn is_interface(&self) -> bool {
        self.access & flags::INTERFACE != 0
    }

    pub fn is_annotation(&self) -> bool {
        self.access & flags::ANNOTATION != 0
    }

    pub fn is_enum(&self) -> bool {
        self.access & flags::ENUM != 0
    }
}

/// A class type named only by its internal name, used where a class file gives
/// no generic signature (`super_class`, `interfaces`, `Exceptions`).
fn raw_class(name: &str) -> SigType {
    SigType::Class {
        name: name.to_string(),
        args: Vec::new(),
        outer: Vec::new(),
    }
}

struct Attributes<'a> {
    signature: Option<&'a str>,
    deprecated: bool,
    exceptions: Vec<String>,
    inner_classes: Vec<InnerClassInfo>,
    source_file: Option<String>,
    param_names: Option<Vec<String>>,
}

fn parse_attributes<'a>(
    reader: &mut Reader<'a>,
    pool: &'a ConstantPool,
    want_members: bool,
) -> Result<Attributes<'a>> {
    let mut out = Attributes {
        signature: None,
        deprecated: false,
        exceptions: Vec::new(),
        inner_classes: Vec::new(),
        source_file: None,
        param_names: None,
    };
    let count = reader.u16()?;
    for _ in 0..count {
        let name_index = reader.u16()?;
        let length = reader.u32()? as usize;
        let name = pool.utf8(name_index).unwrap_or("");
        let body = reader.bytes(length)?;
        let mut body = Reader::new(body);
        match name {
            "Signature" => out.signature = Some(pool.utf8(body.u16()?)?),
            "Deprecated" => out.deprecated = true,
            "Exceptions" => {
                let n = body.u16()?;
                for _ in 0..n {
                    let index = body.u16()?;
                    if !ConstantPool::is_absent(index) {
                        out.exceptions.push(pool.class_name(index)?.to_string());
                    }
                }
            }
            "SourceFile" => out.source_file = Some(pool.utf8(body.u16()?)?.to_string()),
            "MethodParameters" => {
                let n = body.u8()?;
                let mut names = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let index = body.u16()?;
                    let _access = body.u16()?;
                    names.push(if ConstantPool::is_absent(index) {
                        String::new()
                    } else {
                        pool.utf8(index)?.to_string()
                    });
                }
                out.param_names = Some(names);
            }
            "InnerClasses" if want_members => {
                let n = body.u16()?;
                for _ in 0..n {
                    let inner_index = body.u16()?;
                    let outer_index = body.u16()?;
                    let simple_index = body.u16()?;
                    let access = body.u16()?;
                    if ConstantPool::is_absent(inner_index) {
                        continue;
                    }
                    out.inner_classes.push(InnerClassInfo {
                        inner: pool.class_name(inner_index)?.to_string(),
                        outer: if ConstantPool::is_absent(outer_index) {
                            None
                        } else {
                            Some(pool.class_name(outer_index)?.to_string())
                        },
                        simple_name: if ConstantPool::is_absent(simple_index) {
                            None
                        } else {
                            Some(pool.utf8(simple_index)?.to_string())
                        },
                        access,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Decodes a `.class` file. Bodies of methods are skipped entirely: the server
/// needs declarations, so `Code` attributes are never examined.
pub fn parse(bytes: &[u8]) -> Result<ClassInfo> {
    let mut reader = Reader::new(bytes);
    let magic = reader.u32()?;
    if magic != 0xCAFE_BABE {
        return Err(ClassFileError::BadMagic(magic));
    }
    let _minor = reader.u16()?;
    let major_version = reader.u16()?;
    let pool = ConstantPool::parse(&mut reader)?;

    let access = reader.u16()?;
    let this_class = reader.u16()?;
    let internal_name = pool.class_name(this_class)?.to_string();
    let super_index = reader.u16()?;
    let raw_superclass = if ConstantPool::is_absent(super_index) {
        None
    } else {
        Some(raw_class(pool.class_name(super_index)?))
    };
    let interface_count = reader.u16()?;
    let mut raw_interfaces = Vec::with_capacity(interface_count as usize);
    for _ in 0..interface_count {
        raw_interfaces.push(raw_class(pool.class_name(reader.u16()?)?));
    }

    let field_count = reader.u16()?;
    let mut fields = Vec::with_capacity(field_count as usize);
    for _ in 0..field_count {
        let access = reader.u16()?;
        let name = pool.utf8(reader.u16()?)?.to_string();
        let descriptor = pool.utf8(reader.u16()?)?;
        let attrs = parse_attributes(&mut reader, &pool, false)?;
        // A generic signature is authoritative when present; the descriptor is
        // the erasure and loses type arguments.
        let ty = attrs
            .signature
            .and_then(|s| signature::parse_type(s).ok())
            .unwrap_or(signature::parse_type(descriptor)?);
        fields.push(FieldInfo {
            name,
            access,
            ty,
            deprecated: attrs.deprecated,
        });
    }

    let method_count = reader.u16()?;
    let mut methods = Vec::with_capacity(method_count as usize);
    for _ in 0..method_count {
        let access = reader.u16()?;
        let name = pool.utf8(reader.u16()?)?.to_string();
        let descriptor = pool.utf8(reader.u16()?)?;
        let attrs = parse_attributes(&mut reader, &pool, false)?;
        let erased = signature::parse_method(descriptor)?;
        let generic = attrs
            .signature
            .and_then(|s| signature::parse_method(s).ok());
        // A generic signature may omit synthetic leading parameters (an inner
        // class's captured outer instance), so fall back when the counts differ.
        let sig = match generic {
            Some(g) if g.params.len() == erased.params.len() => g,
            _ => erased,
        };
        let mut throws: Vec<SigType> = sig.throws;
        if throws.is_empty() {
            throws = attrs.exceptions.iter().map(|e| raw_class(e)).collect();
        }
        methods.push(MethodInfo {
            name,
            access,
            type_params: sig.type_params,
            params: sig.params,
            ret: sig.ret.unwrap_or(SigType::Primitive('V')),
            throws,
            param_names: attrs.param_names,
            deprecated: attrs.deprecated,
        });
    }

    let attrs = parse_attributes(&mut reader, &pool, true)?;
    let class_sig = attrs.signature.and_then(|s| signature::parse_class(s).ok());
    let (type_params, superclass, interfaces) = match class_sig {
        Some(sig) if sig.interfaces.len() == raw_interfaces.len() => {
            (sig.type_params, sig.superclass, sig.interfaces)
        }
        _ => (Vec::new(), raw_superclass, raw_interfaces),
    };

    Ok(ClassInfo {
        internal_name,
        access,
        type_params,
        superclass,
        interfaces,
        fields,
        methods,
        inner_classes: attrs.inner_classes,
        deprecated: attrs.deprecated,
        source_file: attrs.source_file,
        major_version,
    })
}

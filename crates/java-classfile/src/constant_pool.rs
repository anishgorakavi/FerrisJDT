//! The class file constant pool (JVMS 4.4).

use crate::error::{ClassFileError, Result};
use crate::reader::{decode_modified_utf8, Reader};

/// Only the constant kinds the semantic layer consumes are retained; the rest
/// are recorded as `Other` so indices stay aligned without holding their payload.
#[derive(Debug, Clone)]
pub enum Constant {
    Utf8(Box<str>),
    Class {
        name: u16,
    },
    NameAndType {
        name: u16,
        descriptor: u16,
    },
    /// Occupies a slot but carries nothing the server needs.
    Other,
    /// The unusable second slot of a Long or Double constant (JVMS 4.4.5).
    Unusable,
}

pub struct ConstantPool {
    entries: Vec<Constant>,
}

impl ConstantPool {
    pub fn parse(reader: &mut Reader<'_>) -> Result<ConstantPool> {
        let count = reader.u16()?;
        let mut entries = Vec::with_capacity(count as usize);
        entries.push(Constant::Unusable); // index 0 is never valid
        let mut index = 1u16;
        while index < count {
            let tag = reader.u8()?;
            let constant = match tag {
                1 => {
                    let len = reader.u16()? as usize;
                    Constant::Utf8(decode_modified_utf8(reader.bytes(len)?).into())
                }
                7 => Constant::Class {
                    name: reader.u16()?,
                },
                12 => Constant::NameAndType {
                    name: reader.u16()?,
                    descriptor: reader.u16()?,
                },
                8 | 16 | 19 | 20 => {
                    reader.skip(2)?;
                    Constant::Other
                }
                15 => {
                    reader.skip(3)?;
                    Constant::Other
                }
                3 | 4 | 9 | 10 | 11 | 17 | 18 => {
                    reader.skip(4)?;
                    Constant::Other
                }
                5 | 6 => {
                    reader.skip(8)?;
                    Constant::Other
                }
                _ => return Err(ClassFileError::UnknownTag(tag)),
            };
            let wide = matches!(tag, 5 | 6);
            entries.push(constant);
            index += 1;
            if wide {
                entries.push(Constant::Unusable);
                index += 1;
            }
        }
        Ok(ConstantPool { entries })
    }

    fn get(&self, index: u16) -> Result<&Constant> {
        self.entries
            .get(index as usize)
            .ok_or(ClassFileError::BadConstantIndex(index))
    }

    pub fn utf8(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            Constant::Utf8(s) => Ok(s),
            other => Err(ClassFileError::WrongConstantKind {
                index,
                found: kind_name(other),
                expected: "Utf8",
            }),
        }
    }

    /// Resolves a `CONSTANT_Class` to its internal name, e.g. `java/util/Map$Entry`.
    pub fn class_name(&self, index: u16) -> Result<&str> {
        match self.get(index)? {
            Constant::Class { name } => self.utf8(*name),
            other => Err(ClassFileError::WrongConstantKind {
                index,
                found: kind_name(other),
                expected: "Class",
            }),
        }
    }

    /// `true` when the index is 0, which the format uses to mean "absent"
    /// (for example `super_class` on `java/lang/Object`).
    pub fn is_absent(index: u16) -> bool {
        index == 0
    }
}

fn kind_name(c: &Constant) -> &'static str {
    match c {
        Constant::Utf8(_) => "Utf8",
        Constant::Class { .. } => "Class",
        Constant::NameAndType { .. } => "NameAndType",
        Constant::Other => "unretained constant",
        Constant::Unusable => "unusable slot",
    }
}

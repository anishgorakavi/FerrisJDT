//! Parsers for JVM type descriptors (JVMS 4.3) and generic signatures (JVMS 4.7.9.1).
//!
//! The output is deliberately syntactic: `SigType` mirrors what the class file
//! says and nothing more. Mapping it onto resolved types is the semantic
//! layer's job, which keeps this crate free of any notion of a classpath.

use crate::error::{ClassFileError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigType {
    /// One of `BCDFIJSZV`, exactly as written in the descriptor.
    Primitive(char),
    /// A class type and its type arguments. `outer` carries the enclosing parts
    /// of a qualified inner type such as `Map<K,V>.Entry<K,V>`, outermost first.
    Class {
        /// Internal name with `/` separators, e.g. `java/util/List`.
        name: String,
        args: Vec<SigArg>,
        outer: Vec<(String, Vec<SigArg>)>,
    },
    Array(Box<SigType>),
    /// A reference to a type parameter, e.g. `TE;` becomes `Var("E")`.
    Var(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigArg {
    Exact(SigType),
    Extends(SigType),
    Super(SigType),
    /// The unbounded wildcard `*`.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParamSig {
    pub name: String,
    /// Bounds in declaration order; the class bound comes first when present.
    pub bounds: Vec<SigType>,
}

#[derive(Debug, Clone, Default)]
pub struct ClassSig {
    pub type_params: Vec<TypeParamSig>,
    pub superclass: Option<SigType>,
    pub interfaces: Vec<SigType>,
}

#[derive(Debug, Clone, Default)]
pub struct MethodSig {
    pub type_params: Vec<TypeParamSig>,
    pub params: Vec<SigType>,
    pub ret: Option<SigType>,
    pub throws: Vec<SigType>,
}

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Cursor {
            s: s.as_bytes(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn bump(&mut self) -> Result<u8> {
        let c = self.peek().ok_or_else(|| self.err())?;
        self.i += 1;
        Ok(c)
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.err())
        }
    }

    fn err(&self) -> ClassFileError {
        ClassFileError::BadDescriptor(String::from_utf8_lossy(self.s).into_owned())
    }

    /// Reads an identifier terminated by one of `;<.:`, which is how both
    /// descriptors and signatures delimit every name.
    fn ident(&mut self) -> Result<String> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if matches!(c, b';' | b'<' | b'.' | b':') {
                break;
            }
            self.i += 1;
        }
        if start == self.i {
            return Err(self.err());
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn ty(&mut self) -> Result<SigType> {
        match self.bump()? {
            b'[' => Ok(SigType::Array(Box::new(self.ty()?))),
            b'T' => {
                let name = self.ident()?;
                self.expect(b';')?;
                Ok(SigType::Var(name))
            }
            b'L' => {
                let mut name = self.ident()?;
                let mut args = self.args()?;
                let mut outer = Vec::new();
                // `.` introduces an inner class of the type parsed so far.
                while self.eat(b'.') {
                    outer.push((std::mem::take(&mut name), std::mem::take(&mut args)));
                    name = self.ident()?;
                    args = self.args()?;
                    // Inner names in signatures are simple; rebuild the binary
                    // name so the result matches what the constant pool holds.
                    if let Some((parent, _)) = outer.last() {
                        name = format!("{parent}${name}");
                    }
                }
                self.expect(b';')?;
                Ok(SigType::Class { name, args, outer })
            }
            c if matches!(
                c,
                b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' | b'V'
            ) =>
            {
                Ok(SigType::Primitive(c as char))
            }
            _ => Err(self.err()),
        }
    }

    fn args(&mut self) -> Result<Vec<SigArg>> {
        if !self.eat(b'<') {
            return Ok(Vec::new());
        }
        let mut args = Vec::new();
        loop {
            match self.peek().ok_or_else(|| self.err())? {
                b'>' => {
                    self.i += 1;
                    return Ok(args);
                }
                b'*' => {
                    self.i += 1;
                    args.push(SigArg::Any);
                }
                b'+' => {
                    self.i += 1;
                    args.push(SigArg::Extends(self.ty()?));
                }
                b'-' => {
                    self.i += 1;
                    args.push(SigArg::Super(self.ty()?));
                }
                _ => args.push(SigArg::Exact(self.ty()?)),
            }
        }
    }

    fn type_params(&mut self) -> Result<Vec<TypeParamSig>> {
        if !self.eat(b'<') {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        while !self.eat(b'>') {
            let name = self.ident()?;
            let mut bounds = Vec::new();
            // A single `:` with nothing after it means the class bound is
            // Object and is therefore omitted.
            self.expect(b':')?;
            if !matches!(self.peek(), Some(b':') | Some(b'>') | None) {
                bounds.push(self.ty()?);
            }
            while self.eat(b':') {
                bounds.push(self.ty()?);
            }
            params.push(TypeParamSig { name, bounds });
            if self.peek().is_none() {
                return Err(self.err());
            }
        }
        Ok(params)
    }
}

/// Parses a field descriptor (`Ljava/lang/String;`) or field signature
/// (`Ljava/util/List<TE;>;`); the grammars coincide for a single type.
pub fn parse_type(s: &str) -> Result<SigType> {
    let mut c = Cursor::new(s);
    let ty = c.ty()?;
    if c.i != c.s.len() {
        return Err(c.err());
    }
    Ok(ty)
}

/// Parses a method descriptor (`(ILjava/lang/String;)V`) or a method
/// signature with type parameters and `throws` clauses.
pub fn parse_method(s: &str) -> Result<MethodSig> {
    let mut c = Cursor::new(s);
    let type_params = c.type_params()?;
    c.expect(b'(')?;
    let mut params = Vec::new();
    while !c.eat(b')') {
        if c.peek().is_none() {
            return Err(c.err());
        }
        params.push(c.ty()?);
    }
    let ret = c.ty()?;
    let mut throws = Vec::new();
    while c.eat(b'^') {
        throws.push(c.ty()?);
    }
    if c.i != c.s.len() {
        return Err(c.err());
    }
    Ok(MethodSig {
        type_params,
        params,
        ret: Some(ret),
        throws,
    })
}

/// Parses a class signature: type parameters, superclass, then interfaces.
pub fn parse_class(s: &str) -> Result<ClassSig> {
    let mut c = Cursor::new(s);
    let type_params = c.type_params()?;
    let superclass = Some(c.ty()?);
    let mut interfaces = Vec::new();
    while c.i < c.s.len() {
        interfaces.push(c.ty()?);
    }
    Ok(ClassSig {
        type_params,
        superclass,
        interfaces,
    })
}

//! Tests for type descriptor and generic signature parsing.

use java_classfile::signature::{self, SigArg, SigType};

fn class(name: &str, args: Vec<SigArg>) -> SigType {
    SigType::Class {
        name: name.to_string(),
        args,
        outer: Vec::new(),
    }
}

#[test]
fn parses_primitive_and_array_descriptors() {
    assert_eq!(signature::parse_type("I").unwrap(), SigType::Primitive('I'));
    assert_eq!(
        signature::parse_type("[[J").unwrap(),
        SigType::Array(Box::new(SigType::Array(Box::new(SigType::Primitive('J')))))
    );
    assert_eq!(
        signature::parse_type("[Ljava/lang/String;").unwrap(),
        SigType::Array(Box::new(class("java/lang/String", vec![])))
    );
}

#[test]
fn parses_parameterized_field_signature() {
    let ty =
        signature::parse_type("Ljava/util/Map<Ljava/lang/String;Ljava/util/List<TE;>;>;").unwrap();
    let SigType::Class { name, args, .. } = ty else {
        panic!("expected a class type");
    };
    assert_eq!(name, "java/util/Map");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], SigArg::Exact(class("java/lang/String", vec![])));
    assert_eq!(
        args[1],
        SigArg::Exact(class(
            "java/util/List",
            vec![SigArg::Exact(SigType::Var("E".into()))]
        ))
    );
}

#[test]
fn parses_wildcards() {
    let ty = signature::parse_type("Ljava/util/List<+Ljava/lang/Number;>;").unwrap();
    let SigType::Class { args, .. } = ty else {
        panic!()
    };
    assert_eq!(args[0], SigArg::Extends(class("java/lang/Number", vec![])));

    let ty = signature::parse_type("Ljava/lang/Comparable<-TE;>;").unwrap();
    let SigType::Class { args, .. } = ty else {
        panic!()
    };
    assert_eq!(args[0], SigArg::Super(SigType::Var("E".into())));

    let ty = signature::parse_type("Ljava/util/List<*>;").unwrap();
    let SigType::Class { args, .. } = ty else {
        panic!()
    };
    assert_eq!(args[0], SigArg::Any);
}

#[test]
fn parses_qualified_inner_type() {
    let ty = signature::parse_type("Ljava/util/Map<TK;TV;>.Entry<TK;TV;>;").unwrap();
    let SigType::Class { name, args, outer } = ty else {
        panic!()
    };
    // The inner type is rebuilt as a binary name so classpath lookups match.
    assert_eq!(name, "java/util/Map$Entry");
    assert_eq!(args.len(), 2);
    assert_eq!(outer.len(), 1);
    assert_eq!(outer[0].0, "java/util/Map");
}

#[test]
fn parses_method_descriptor() {
    let m = signature::parse_method("(ILjava/lang/String;[Z)V").unwrap();
    assert_eq!(m.params.len(), 3);
    assert_eq!(m.params[0], SigType::Primitive('I'));
    assert_eq!(m.ret, Some(SigType::Primitive('V')));
    assert!(m.type_params.is_empty());
}

#[test]
fn parses_generic_method_signature_with_bounds_and_throws() {
    // Collections.max: <T extends Object & Comparable<? super T>> T max(Collection<? extends T>)
    let m = signature::parse_method(
        "<T::Ljava/lang/Comparable<-TT;>;>(Ljava/util/Collection<+TT;>;)TT;^Ljava/io/IOException;",
    )
    .unwrap();
    assert_eq!(m.type_params.len(), 1);
    assert_eq!(m.type_params[0].name, "T");
    // A bare `::` means the class bound is Object and only the interface
    // bound is recorded.
    assert_eq!(m.type_params[0].bounds.len(), 1);
    assert_eq!(m.ret, Some(SigType::Var("T".into())));
    assert_eq!(m.throws.len(), 1);
}

#[test]
fn parses_class_signature() {
    let sig = signature::parse_class(
        "<E:Ljava/lang/Object;>Ljava/util/AbstractList<TE;>;Ljava/util/List<TE;>;Ljava/io/Serializable;",
    )
    .unwrap();
    assert_eq!(sig.type_params.len(), 1);
    assert_eq!(sig.type_params[0].name, "E");
    assert_eq!(
        sig.type_params[0].bounds,
        vec![class("java/lang/Object", vec![])]
    );
    assert_eq!(
        sig.superclass,
        Some(class(
            "java/util/AbstractList",
            vec![SigArg::Exact(SigType::Var("E".into()))]
        ))
    );
    assert_eq!(sig.interfaces.len(), 2);
}

#[test]
fn rejects_malformed_signatures_without_panicking() {
    for bad in [
        "",
        "L",
        "Ljava/lang/String",
        "(I",
        "Q",
        "<",
        "<T",
        "<T:",
        "Ljava/util/List<",
        "TE",
        "[",
        "()",
        "(V",
        "Ljava/util/Map<TK;TV;>.",
        "\u{0}",
        "LL;;",
    ] {
        // Either outcome is acceptable; crashing is not.
        let _ = signature::parse_type(bad);
        let _ = signature::parse_method(bad);
        let _ = signature::parse_class(bad);
    }
}

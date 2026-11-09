//! Does the semantic layer actually resolve what it claims to?
//!
//! Each test states a Java fragment and the type that must come out of it.
//! Several are the exact cases from the acceptance brief.

use java_semantic::fixture::Fixture;
use java_semantic::infer::Inferer;
use java_semantic::scope::context_at;
use java_semantic::{LookupOptions, Type};
use java_syntax::ast;

/// Infers the type of the expression ending just before the `$0` marker.
fn type_at(files: &[(&str, &str)]) -> String {
    let fixture = Fixture::new(files);
    let (file, offset) = fixture.cursor();
    let context = context_at(&fixture.db, file, offset).expect("a context at the cursor");
    let inferer = Inferer::new(&context);
    // The widest expression ending at the cursor, so that `items.get(0)$0`
    // is read as the whole call rather than just its closing token.
    match ast::expression_ending_at(context.source.tree(), offset) {
        Some(expression) => inferer.type_of(expression).display_short(),
        None => "<no expression>".to_string(),
    }
}

/// Wraps a method body in a class so tests can stay short.
fn in_method(body: &str) -> String {
    format!("import java.util.*;\npublic class T {{\n  void m() {{\n{body}\n  }}\n}}\n")
}

#[test]
fn infers_literals_and_operators() {
    let cases = [
        ("int x = 1$0;", "int"),
        ("long x = 1L$0;", "long"),
        ("double d = 1.5$0;", "double"),
        ("float f = 1.5f$0;", "float"),
        ("char c = 'a'$0;", "char"),
        ("boolean b = true$0;", "boolean"),
        ("String s = \"hi\"$0;", "String"),
        ("Object o = null$0;", "null"),
        ("int x = 1 + 2$0;", "int"),
        ("long x = 1 + 2L$0;", "long"),
        ("double x = 1 + 2.0$0;", "double"),
        ("String s = \"a\" + 1$0;", "String"),
        ("String s = 1 + \"a\"$0;", "String"),
        ("boolean b = 1 < 2$0;", "boolean"),
        ("boolean b = !true$0;", "boolean"),
        ("int x = 'a' + 1$0;", "int"),
        ("int x = (int) 1.5$0;", "int"),
        ("boolean b = \"a\" instanceof String$0;", "boolean"),
        ("int[] a = new int[3]; int y = a[0]$0;", "int"),
        ("int[] a = new int[3]; int n = a.length$0;", "int"),
        ("int[][] g = new int[2][2]; int[] r = g[0]$0;", "int[]"),
        ("byte b = 1; int x = b + b$0;", "int"),
        ("int x = 1; long y = x << 2$0;", "int"),
    ];
    for (body, expected) in cases {
        assert_eq!(
            type_at(&[("T.java", &in_method(body))]),
            expected,
            "for {body}"
        );
    }
}

#[test]
fn infers_local_variables_and_parameters() {
    let source = "public class T {\n  void m(String name, int count) {\n    String local = name;\n    int c = count$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "int");
    let source = "public class T {\n  void m(String name) {\n    String local = name$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "String");
}

#[test]
fn infers_fields_and_this() {
    let source = "public class T {\n  private int size;\n  static String label;\n  void m() {\n    int s = size$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "int");
    let source =
        "public class T {\n  private int size;\n  void m() {\n    int s = this.size$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "int");
    let source = "public class T {\n  void m() {\n    T self = this$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "T");
}

#[test]
fn infers_standard_library_generics() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    if !fixture.has_stdlib() {
        eprintln!("skipping: no JDK standard library indexed");
        return;
    }
    drop(fixture);
    // The core promise: a generic receiver yields substituted member types.
    let cases = [
        (
            "List<String> xs = new ArrayList<>(); String s = xs.get(0)$0;",
            "String",
        ),
        (
            "List<String> xs = new ArrayList<>(); int n = xs.size()$0;",
            "int",
        ),
        (
            "List<String> xs = new ArrayList<>(); boolean b = xs.add(\"a\")$0;",
            "boolean",
        ),
        (
            "List<String> xs = new ArrayList<>(); Iterator<String> i = xs.iterator()$0;",
            "Iterator<String>",
        ),
        (
            "Map<String, Integer> m = new HashMap<>(); Integer v = m.get(\"k\")$0;",
            "Integer",
        ),
        (
            "Map<String, Integer> m = new HashMap<>(); Set<String> k = m.keySet()$0;",
            "Set<String>",
        ),
        (
            "List<List<String>> n = new ArrayList<>(); List<String> i = n.get(0)$0;",
            "List<String>",
        ),
        (
            "ArrayList<String> a = new ArrayList<>()$0;",
            "ArrayList<String>",
        ),
        ("String s = \"a\"; int n = s.length()$0;", "int"),
        ("String s = \"a\"; String t = s.substring(1)$0;", "String"),
        ("String s = \"a\"; char c = s.charAt(0)$0;", "char"),
        ("String[] parts = \"a,b\".split(\",\")$0;", "String[]"),
        ("int n = Integer.parseInt(\"1\")$0;", "int"),
        ("String s = String.valueOf(1)$0;", "String"),
        ("double d = Math.max(1.0, 2.0)$0;", "double"),
        ("int n = Math.max(1, 2)$0;", "int"),
        (
            "StringBuilder b = new StringBuilder(); StringBuilder c = b.append(\"x\")$0;",
            "StringBuilder",
        ),
        (
            "Map.Entry<String, Integer> e = null; String k = e.getKey()$0;",
            "String",
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(
            type_at(&[("T.java", &in_method(body))]),
            expected,
            "for {body}"
        );
    }
}

#[test]
fn infers_enhanced_for_variables() {
    let source =
        in_method("List<String> xs = new ArrayList<>();\nfor (String x : xs) { String y = x$0; }");
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    assert_eq!(type_at(&[("T.java", &source)]), "String");
}

#[test]
fn resolves_overloads_by_argument_type() {
    let source = "public class T {\n  String f(int x) { return null; }\n  int f(String x) { return 0; }\n  int f(Object x) { return 1; }\n  void m() {\n    String a = f(1)$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "String");
    let source = "public class T {\n  String f(int x) { return null; }\n  int f(String x) { return 0; }\n  void m() {\n    int a = f(\"s\")$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "int");
    // The most specific overload wins: String is more specific than Object.
    let source = "public class T {\n  int f(Object x) { return 0; }\n  boolean f(String x) { return true; }\n  void m() {\n    boolean a = f(\"s\")$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "boolean");
    // Boxing is only considered when no strict overload applies.
    let source = "public class T {\n  int f(Integer x) { return 0; }\n  boolean f(long x) { return true; }\n  void m() {\n    boolean a = f(1)$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "boolean");
    // Varargs is the last resort.
    let source = "public class T {\n  int f(String... x) { return 0; }\n  boolean f(String x) { return true; }\n  void m() {\n    boolean a = f(\"a\")$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "boolean");
    let source = "public class T {\n  int f(String... x) { return 0; }\n  boolean f(String x) { return true; }\n  void m() {\n    int a = f(\"a\", \"b\")$0;\n  }\n}";
    assert_eq!(type_at(&[("T.java", source)]), "int");
}

#[test]
fn resolves_inheritance_across_files() {
    let files = [
        ("Base.java", "public class Base {\n  protected int shared;\n  public String describe() { return null; }\n}"),
        ("Derived.java", "public class Derived extends Base {\n  void m() {\n    String s = describe()$0;\n  }\n}"),
    ];
    assert_eq!(type_at(&files), "String");
    let files = [
        (
            "Base.java",
            "public class Base {\n  protected int shared;\n}",
        ),
        (
            "Derived.java",
            "public class Derived extends Base {\n  void m() {\n    int s = shared$0;\n  }\n}",
        ),
    ];
    assert_eq!(type_at(&files), "int");
    // An inherited member reached through a variable of the subclass type.
    let files = [
        ("Base.java", "public class Base {\n  public String describe() { return null; }\n}"),
        ("Derived.java", "public class Derived extends Base {}"),
        ("T.java", "public class T {\n  void m() {\n    Derived d = new Derived();\n    String s = d.describe()$0;\n  }\n}"),
    ];
    assert_eq!(type_at(&files), "String");
}

#[test]
fn resolves_generic_class_declared_in_the_workspace() {
    let files = [(
        "Box.java",
        "public class Box<T> {\n  private T value;\n  public T get() { return value; }\n  public void set(T v) { value = v; }\n}",
    ), (
        "T.java",
        "public class T {\n  void m() {\n    Box<String> b = new Box<String>();\n    String s = b.get()$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "String");
    // Nested generics through a workspace class.
    let files = [(
        "Box.java",
        "public class Box<T> {\n  public T get() { return null; }\n}",
    ), (
        "T.java",
        "public class T {\n  void m() {\n    Box<Box<String>> b = null;\n    Box<String> s = b.get()$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "Box<String>");
}

/// The exact acceptance case from the brief.
#[test]
fn binary_search_tree_resolves_its_own_members() {
    let files = [(
        "BinarySearchTree.java",
        "class BSTNode<E> {\n    E data;\n    BSTNode<E> left;\n    BSTNode<E> right;\n    BSTNode(E data) { this.data = data; }\n}\n\nclass BinarySearchTree<E extends Comparable<? super E>> {\n    private BSTNode<E> root;\n    private int size;\n\n    public boolean add(E value) {\n        BSTNode<E> node = root$0;\n        return false;\n    }\n}\n",
    )];
    // `root` is a field of type BSTNode<E>.
    assert_eq!(type_at(&files), "BSTNode<E>");
}

#[test]
fn type_parameter_resolves_its_bound_members() {
    let files = [(
        "BinarySearchTree.java",
        "class BinarySearchTree<E extends Comparable<? super E>> {\n    public boolean add(E value, E other) {\n        int c = value.compareTo(other)$0;\n        return c > 0;\n    }\n}\n",
    )];
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        eprintln!("skipping: compareTo needs java.lang.Comparable");
        return;
    }
    // compareTo is inherited from the bound Comparable<? super E> and returns int.
    assert_eq!(type_at(&files), "int");
}

#[test]
fn node_field_offers_only_its_own_members() {
    let files = [(
        "Tree.java",
        "class BSTNode<E> {\n    E data;\n    BSTNode<E> left;\n    BSTNode<E> right;\n    int height() { return 0; }\n}\n\nclass Tree<E extends Comparable<? super E>> {\n    private BSTNode<E> root;\n    void m() {\n        root.$0\n    }\n}\n",
    )];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let context = context_at(&fixture.db, file, offset).unwrap();
    let inferer = Inferer::new(&context);
    let root = context.resolve_name("root").expect("root resolves");
    let ty = root.value_type().expect("root has a type");
    assert_eq!(ty.display_short(), "BSTNode<E>");
    let members = inferer.members_for(&ty, LookupOptions::instance());

    let field_names: Vec<&str> = members.fields.iter().map(|f| &*f.field.name).collect();
    assert!(field_names.contains(&"data"), "{field_names:?}");
    assert!(field_names.contains(&"left"), "{field_names:?}");
    assert!(field_names.contains(&"right"), "{field_names:?}");
    // Members of unrelated types must not appear.
    assert!(!field_names.contains(&"root"), "{field_names:?}");
    assert!(!field_names.contains(&"size"), "{field_names:?}");

    let method_names: Vec<&str> = members.methods.iter().map(|m| &*m.method.name).collect();
    assert!(method_names.contains(&"height"), "{method_names:?}");
    // Inherited from Object, which every class has.
    if fixture.has_stdlib() {
        assert!(method_names.contains(&"toString"), "{method_names:?}");
    }

    // `data` is the element type, substituted from BSTNode<E>.
    assert_eq!(members.field("data").unwrap().field.ty.display_short(), "E");
}

#[test]
fn generic_substitution_reaches_through_the_hierarchy() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    if !fixture.has_stdlib() {
        return;
    }
    // ArrayList<String> must present add(String), inherited through
    // AbstractList<E> and List<E>, not add(E) and not add(Object).
    let ty = Type::generic("java.util.ArrayList", vec![Type::string()]);
    let members = java_semantic::members::members_of(&fixture.db, &ty, LookupOptions::instance());
    let add = *members
        .methods_named("add")
        .iter()
        .find(|m| m.method.params.len() == 1)
        .expect("add(E) is present");
    assert_eq!(add.method.params[0].ty.display_short(), "String");
    let get = members
        .methods_named("get")
        .first()
        .copied()
        .expect("get(int)");
    assert_eq!(get.method.ret.display_short(), "String");
    let iterator = members.methods_named("iterator").first().copied().unwrap();
    assert_eq!(iterator.method.ret.display_short(), "Iterator<String>");

    // The members a student expects on a list are all reachable.
    for expected in [
        "add", "get", "size", "contains", "remove", "iterator", "isEmpty", "clear",
    ] {
        assert!(
            members.methods.iter().any(|m| &*m.method.name == expected),
            "ArrayList<String> should offer {expected}"
        );
    }
}

#[test]
fn raw_types_still_offer_members() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    if !fixture.has_stdlib() {
        return;
    }
    // A raw List erases its parameter rather than leaving it unresolved.
    let ty = Type::class("java.util.List");
    let members = java_semantic::members::members_of(&fixture.db, &ty, LookupOptions::instance());
    let get = members
        .methods_named("get")
        .first()
        .copied()
        .expect("get(int)");
    assert_eq!(get.method.ret.display_short(), "Object");
}

#[test]
fn unresolvable_names_stay_unknown() {
    // The key honesty property: nothing is invented for code that does not
    // resolve, so nothing downstream can report a false result.
    let cases = [
        "Missing m = null; Object o = m.anything()$0;",
        "Object o = undefinedVariable$0;",
        "Object o = Undefined.CONSTANT$0;",
    ];
    for body in cases {
        assert_eq!(
            type_at(&[("T.java", &in_method(body))]),
            "<unknown>",
            "for {body}"
        );
    }
}

#[test]
fn nested_and_static_nested_classes_resolve() {
    let files = [(
        "Outer.java",
        "public class Outer {\n  static class Inner {\n    int value;\n  }\n  class NonStatic {\n    String label;\n  }\n  void m() {\n    Inner i = new Inner();\n    int v = i.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
    let files = [(
        "Outer.java",
        "public class Outer {\n  static class Inner {\n    int value;\n  }\n}",
    ), (
        "T.java",
        "public class T {\n  void m() {\n    Outer.Inner i = null;\n    int v = i.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
}

#[test]
fn interfaces_and_default_methods_resolve() {
    let files = [(
        "Shape.java",
        "public interface Shape {\n  double area();\n  default String describe() { return \"shape\"; }\n}",
    ), (
        "Circle.java",
        "public class Circle implements Shape {\n  public double area() { return 0; }\n  void m() {\n    String d = describe()$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "String");
}

#[test]
fn enums_resolve_their_constants_and_methods() {
    let files = [
        (
            "Color.java",
            "public enum Color {\n  RED, GREEN;\n  int rank() { return 0; }\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Color c = Color.RED$0;\n  }\n}",
        ),
    ];
    assert_eq!(type_at(&files), "Color");
    let files = [
        (
            "Color.java",
            "public enum Color { RED, GREEN; int rank() { return 0; } }",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    int r = Color.RED.rank()$0;\n  }\n}",
        ),
    ];
    assert_eq!(type_at(&files), "int");
}

#[test]
fn packages_and_imports_resolve_across_directories() {
    let files = [(
        "src/ds/Node.java",
        "package ds;\npublic class Node {\n  public int value;\n}",
    ), (
        "src/app/Main.java",
        "package app;\nimport ds.Node;\npublic class Main {\n  void m() {\n    Node n = new Node();\n    int v = n.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
    // Wildcard import.
    let files = [(
        "src/ds/Node.java",
        "package ds;\npublic class Node { public int value; }",
    ), (
        "src/app/Main.java",
        "package app;\nimport ds.*;\npublic class Main {\n  void m() {\n    Node n = new Node();\n    int v = n.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
    // Fully qualified, with no import at all.
    let files = [(
        "src/ds/Node.java",
        "package ds;\npublic class Node { public int value; }",
    ), (
        "src/app/Main.java",
        "package app;\npublic class Main {\n  void m() {\n    ds.Node n = new ds.Node();\n    int v = n.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
}

#[test]
fn constructors_resolve_including_overloads() {
    let files = [
        (
            "Pair.java",
            "public class Pair {\n  Pair() {}\n  Pair(int a) {}\n  Pair(String a) {}\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Pair p = new Pair(1)$0;\n  }\n}",
        ),
    ];
    assert_eq!(type_at(&files), "Pair");

    let fixture = Fixture::new(&[
        (
            "Pair.java",
            "public class Pair {\n  Pair() {}\n  Pair(int a) {}\n  Pair(String a) {}\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Pair p = new Pair(1$0);\n  }\n}",
        ),
    ]);
    let (file, offset) = fixture.cursor();
    let context = context_at(&fixture.db, file, offset).unwrap();
    let inferer = Inferer::new(&context);
    let call = ast::ancestor_of_kind(
        ast::named_node_at(context.source.tree(), offset),
        &["object_creation_expression"],
    )
    .expect("inside a constructor call");
    let chosen = inferer.resolve_invocation(call).expect("a constructor");
    assert_eq!(chosen.method.params.len(), 1);
    assert_eq!(chosen.method.params[0].ty.display_short(), "int");
}

#[test]
fn recursive_and_cyclic_declarations_terminate() {
    // A self-referential node type, which every linked structure uses.
    let files = [(
        "Node.java",
        "public class Node {\n  Node next;\n  int value;\n  void m() {\n    int v = next.next.next.value$0;\n  }\n}",
    )];
    assert_eq!(type_at(&files), "int");
    // Mutually recursive classes must not loop.
    let files = [
        ("A.java", "public class A { B b; }"),
        ("B.java", "public class B { A a; int v; }"),
        (
            "T.java",
            "public class T { void m() { A a = new A(); int x = a.b.a.b.v$0; } }",
        ),
    ];
    assert_eq!(type_at(&files), "int");
}

#[test]
fn inference_survives_incomplete_code() {
    // None of these may panic; returning <unknown> is the correct outcome.
    let cases = [
        "public class T { void m() { int x = $0 } }",
        "public class T { void m( $0 }",
        "public class T { void m() { foo.$0 } }",
        "public class T { void m() { new $0 } }",
        "public class T { void m() { x.y.z.$0 } }",
        "public class T { $0",
        "$0",
    ];
    for source in cases {
        let fixture = Fixture::new(&[("T.java", source)]);
        let (file, offset) = fixture.cursor();
        if let Some(context) = context_at(&fixture.db, file, offset) {
            let inferer = Inferer::new(&context);
            let node = ast::named_node_at(context.source.tree(), offset);
            let _ = inferer.type_of(node);
            let _ = context.resolve_name("x");
        }
    }
}

/// A project newer than any release `ct.sym` records (a Java 21 Spring project on a JDK 21)
/// must still see `java.lang`.
#[test]
fn reads_the_standard_library_for_a_release_newer_than_ct_sym() {
    let baseline = Fixture::new(&[("T.java", "class T {}")]);
    if !baseline.has_stdlib() {
        eprintln!("skipping: no JDK standard library indexed");
        return;
    }
    let config = project_model::Config {
        source_level: Some(21),
        ..Default::default()
    };
    let fixture = Fixture::with_config(&[("T.java", "class T {}")], config);
    assert!(fixture.db.classpath().contains("java.lang.String"));
}

/// The major version in a JDK home's `release` file, e.g. `JAVA_VERSION="21.0.13"` is 21.
fn jdk_major(home: &std::path::Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let line = text.lines().find(|l| l.starts_with("JAVA_VERSION="))?;
    let version = line.trim_start_matches("JAVA_VERSION=").trim_matches('"');
    let first = version.split(['.', '-', '+']).next()?;
    // Java 8 reports "1.8.0_x".
    if first == "1" {
        version.split('.').nth(1)?.parse().ok()
    } else {
        first.parse().ok()
    }
}

/// `ct.sym` has no entry for the JDK's own release, so APIs added in it, such as
/// `List.getFirst`, must come from the `lib/modules` image.
#[test]
fn apis_added_in_the_jdks_own_release_are_visible() {
    let baseline = Fixture::new(&[("T.java", "class T {}")]);
    let Some(jdk) = baseline.db.project().jdk.clone() else {
        eprintln!("skipping: no JDK found");
        return;
    };
    let Some(major) = jdk_major(&jdk.home).filter(|m| *m >= 21) else {
        eprintln!("skipping: JDK is older than 21");
        return;
    };
    let config = project_model::Config {
        source_level: Some(major),
        ..Default::default()
    };
    let fixture = Fixture::with_config(&[("T.java", "class T {}")], config);
    let list = fixture
        .db
        .class("java.util.List")
        .expect("java.util.List is indexed");
    assert!(
        list.methods.iter().any(|m| &*m.name == "getFirst"),
        "List.getFirst (Java 21) should be visible at source level {major}"
    );
    assert!(fixture
        .db
        .classpath()
        .contains("java.util.SequencedCollection"));
    // The overlay must not leak JDK-internal packages.
    assert!(!fixture.db.classpath().contains("jdk.internal.misc.Unsafe"));
    assert!(!fixture
        .db
        .classpath()
        .contains("sun.nio.ch.FileChannelImpl"));
}

#[test]
fn static_interface_methods_are_not_inherited() {
    let config = project_model::Config {
        source_level: Some(21),
        ..project_model::Config::default()
    };
    let fixture = Fixture::with_config(&[("T.java", "class T {}")], config);
    if !fixture.has_stdlib() {
        return;
    }
    // `List.of` belongs to List alone (JLS 8.4.8); an implementing class does not have it.
    let list = Type::generic("java.util.List", vec![Type::string()]);
    let on_list = java_semantic::members::members_of(&fixture.db, &list, LookupOptions::default());
    assert!(!on_list.methods_named("of").is_empty());
    let array_list = Type::generic("java.util.ArrayList", vec![Type::string()]);
    let on_class =
        java_semantic::members::members_of(&fixture.db, &array_list, LookupOptions::default());
    assert!(on_class.methods_named("of").is_empty());
    assert!(on_class.methods_named("copyOf").is_empty());
}

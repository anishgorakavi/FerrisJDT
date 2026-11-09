//! Records and pattern matching: valid Java 16+ must not be flagged, members must be offered, and
//! invalid uses must still be reported.

use java_ide::completion::{self, CompletionSettings};
use java_ide::diagnostics;
use java_semantic::fixture::Fixture;
use project_model::Config;

fn config() -> Config {
    Config {
        source_level: Some(21),
        ..Config::default()
    }
}

fn labels(source: &str) -> Vec<String> {
    let mut fixture = Fixture::with_config(&[("T.java", source)], config());
    let (file, offset) = fixture.cursor();
    let settings = CompletionSettings {
        snippets: true,
        additional_edits: true,
    };
    completion::completions_repairing(&mut fixture.db, file, offset, settings)
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn errors(source: &str) -> Vec<String> {
    let fixture = Fixture::with_config(&[("T.java", source)], config());
    let file = fixture.file("T.java");
    diagnostics::diagnostics(&fixture.db, file)
        .iter()
        .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
        .map(diagnostics::describe)
        .collect()
}

fn has_stdlib() -> bool {
    Fixture::new(&[("T.java", "class T {}")]).has_stdlib()
}

const RECORDS: &str = r#"
public class T {
    record Point(int x, int y) {
        Point {
            if (x < 0) throw new IllegalArgumentException("x " + x);
        }
        int sum() { return x + y; }
        static Point origin() { return new Point(0, 0); }
    }
    record Box<A>(A value, String... labels) {}

    void use() {
        Point p = new Point(1, 2);
        int a = p.x() + p.y() + p.sum();
        Box<String> b = new Box<>("v", "a", "b");
        String v = b.value();
        String s = p.toString();
    }
}
"#;

#[test]
fn records_have_components_accessors_and_a_canonical_constructor() {
    if !has_stdlib() {
        return;
    }
    assert_eq!(errors(RECORDS), Vec::<String>::new());
}

#[test]
fn record_accessors_are_offered_and_typed() {
    let source = RECORDS.replace("String s = p.toString();", "p.$0");
    let found = labels(&source);
    for expected in ["x", "y", "sum", "equals", "hashCode", "toString"] {
        assert!(
            found.contains(&expected.to_string()),
            "missing {expected}: {found:?}"
        );
    }
    if has_stdlib() {
        let chained = RECORDS.replace("String s = p.toString();", "b.value().$0");
        assert!(labels(&chained).contains(&"length".to_string()));
    }
}

#[test]
fn a_record_constructor_with_the_wrong_arity_is_reported() {
    if !has_stdlib() {
        return;
    }
    let source =
        "public class T {\n  record P(int x, int y) {}\n  void m() { P p = new P(1); }\n}\n";
    assert!(!errors(source).is_empty());
}

const PATTERNS: &str = r#"
public class T {
    sealed interface Shape permits Circle, Rect {}
    record Circle(double radius) implements Shape {}
    record Rect(double w, double h) implements Shape {}
    record Wrapper(Shape inner) {}

    static double area(Shape s) {
        return switch (s) {
            case Circle c -> c.radius() * c.radius();
            case Rect(double w, var h) -> w * h;
        };
    }

    static String text(Object o) {
        if (o instanceof String str && str.length() > 3) {
            return str.toUpperCase();
        }
        if (!(o instanceof Integer n)) {
            return "other";
        }
        return "int " + n.intValue();
    }

    static double unwrap(Object o) {
        if (o instanceof Wrapper(Circle(var r))) {
            return r;
        }
        return switch (o) {
            case Integer i when i > 10 -> i.doubleValue();
            case null, default -> 0;
        };
    }
}
"#;

#[test]
fn pattern_bindings_are_in_scope() {
    if !has_stdlib() {
        return;
    }
    assert_eq!(errors(PATTERNS), Vec::<String>::new());
}

#[test]
fn pattern_bindings_complete_with_their_type() {
    let source = PATTERNS.replace("return str.toUpperCase();", "return str.$0;");
    assert!(labels(&source).contains(&"toUpperCase".to_string()));
    let switch = PATTERNS.replace("c.radius() * c.radius()", "c.$0");
    assert!(labels(&switch).contains(&"radius".to_string()));
    let record_var = PATTERNS.replace("w * h;", "h.$0;");
    // `var h` takes the record component's type, a primitive: no members, but not Unknown junk.
    assert!(!labels(&record_var).contains(&"toUpperCase".to_string()));
}

#[test]
fn a_name_outside_every_pattern_scope_is_still_unknown() {
    if !has_stdlib() {
        return;
    }
    let source = "public class T {\n  void m(Object o) {\n    if (o instanceof String s) {}\n    int n = missing + 1;\n  }\n}\n";
    let found = errors(source);
    assert!(found.iter().any(|e| e.contains("missing")), "{found:?}");
    assert!(
        !found.iter().any(|e| e.contains('s') && e.contains("`s`")),
        "{found:?}"
    );
}

/// `javac`-verified files exercising generics, enums with bodies, inner classes, overloads,
/// streams, records and patterns. Any diagnostic here is a false positive.
#[test]
fn javac_valid_enterprise_fixtures_produce_no_diagnostics() {
    if !has_stdlib() {
        return;
    }
    for (name, source) in [
        (
            "Repo.java",
            include_str!("../../../tests/fixtures/enterprise/Repo.java"),
        ),
        (
            "Shapes.java",
            include_str!("../../../tests/fixtures/enterprise/Shapes.java"),
        ),
    ] {
        let fixture = Fixture::with_config(&[(name, source)], config());
        let file = fixture.file(name);
        let found: Vec<String> = diagnostics::diagnostics(&fixture.db, file)
            .iter()
            .map(diagnostics::describe)
            .collect();
        assert!(found.is_empty(), "{name}: {found:?}");
    }
}

#[test]
fn enums_have_values_and_value_of() {
    let source = "public class T {\n  enum Level { LOW, HIGH }\n  void m() {\n    Level[] all = Level.values();\n    Level l = Level.valueOf(\"LOW\");\n    l.$0\n  }\n}\n";
    let found = labels(source);
    assert!(found.contains(&"name".to_string()) && found.contains(&"ordinal".to_string()));
    if has_stdlib() {
        assert_eq!(errors(&source.replace("l.$0", "")), Vec::<String>::new());
    }
}

#[test]
fn qualified_inner_class_creation_resolves_against_the_outer_instance() {
    let source = "public class T {\n  class Inner { int twice() { return 2; } }\n  void m(T outer) {\n    Inner in = outer.new Inner();\n    in.$0\n  }\n}\n";
    assert!(labels(source).contains(&"twice".to_string()));
    if has_stdlib() {
        assert_eq!(errors(&source.replace("in.$0", "")), Vec::<String>::new());
    }
}

#[test]
fn try_with_resources_variables_are_in_scope_in_the_body() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.util.stream.*;\nimport java.nio.file.*;\nimport java.io.*;\npublic class T {\n  void m(Path root) throws IOException {\n    try (Stream<Path> stream = Files.walk(root); BufferedReader reader = Files.newBufferedReader(root)) {\n      long n = stream.count();\n      String line = reader.readLine();\n    }\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
    let completion = source.replace("stream.count();", "stream.$0;");
    assert!(labels(&completion).contains(&"filter".to_string()));
}

#[test]
fn an_unbounded_wildcard_captures_the_declared_bound() {
    let source = "public class T {\n  interface Marker { String tag(); }\n  interface Desc<X extends Marker> { X get(); }\n  void m(Desc<?> d) {\n    d.get().$0\n  }\n}\n";
    assert!(labels(source).contains(&"tag".to_string()));
}

#[test]
fn a_fixed_arity_generic_overload_beats_a_varargs_one() {
    if !has_stdlib() {
        return;
    }
    // Mockito's shape, in the order its class file lists them: mock(Class<T>) must win over
    // mock(T...) for a class literal, whatever the declaration order.
    let source = "public class T {\n  interface Foo { int x(); }\n  @SafeVarargs static <X> X mock(X... reified) { return null; }\n  static <X> X mock(Class<X> c) { return null; }\n  static <X> X mock(String name) { return null; }\n  void m() {\n    Foo f = mock(Foo.class);\n    int v = f.x();\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn annotation_types_satisfy_an_annotation_bound() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.lang.annotation.Annotation;\npublic class T {\n  @interface Marker {}\n  static <A extends Annotation> A find(Class<?> c, Class<A> type) { return null; }\n  void m(Class<?> k) {\n    Marker mk = find(k, Marker.class);\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn an_unrelated_class_never_matches_a_parameter_with_unsolved_type_arguments() {
    if !has_stdlib() {
        return;
    }
    // AssertJ's shape: assertThat(Predicate<T>) must not capture a List argument.
    let source = "import java.util.*;\nimport java.util.function.Predicate;\npublic class T {\n  static class PredicateAssert<X> { void hasName() {} }\n  static class ListAssert<E> { void hasSize(int n) {} }\n  static <X> PredicateAssert<X> assertThat(Predicate<X> p) { return null; }\n  static <E> ListAssert<E> assertThat(List<? extends E> l) { return null; }\n  void m(List<String> names) {\n    assertThat(names).hasSize(1);\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
    assert!(
        labels(&source.replace("assertThat(names).hasSize(1);", "assertThat(names).$0"))
            .contains(&"hasSize".to_string())
    );
}

const SELF_TYPED: &str = "import java.util.*;\nimport java.util.function.Predicate;\npublic class T {\n  interface Assert<S extends Assert<S, E>, E> { S hasSize(int n); S allMatch(Predicate<? super E> p); }\n  static class ObjectAssert<X> implements Assert<ObjectAssert<X>, X> {\n    public ObjectAssert<X> hasSize(int n) { return this; }\n    public ObjectAssert<X> allMatch(Predicate<? super X> p) { return this; }\n  }\n  static <X> ObjectAssert<X> assertThat(X actual) { return null; }\n  static <E> Assert<?, E> assertThat(List<? extends E> actual) { return null; }\n  void m(List<String> names) {\n    assertThat(names).allMatch(v -> v.length() > 1);\n    assertThat(names).hasSize(1).allMatch(v -> v.isEmpty());\n  }\n}\n";

#[test]
fn a_specific_overload_beats_a_bare_type_variable_one() {
    if !has_stdlib() {
        return;
    }
    // `assertThat(List<? extends E>)` is more specific than `assertThat(T)`.
    assert_eq!(errors(SELF_TYPED), Vec::<String>::new());
    let source = SELF_TYPED.replace(
        "assertThat(names).hasSize(1).allMatch(v -> v.isEmpty());",
        "assertThat(names).$0",
    );
    assert!(labels(&source).contains(&"allMatch".to_string()));
}

#[test]
fn an_open_wildcard_keeps_the_other_type_arguments_for_a_self_typed_chain() {
    if !has_stdlib() {
        return;
    }
    // After `Assert<?, E>` the element type stays known inside the lambda...
    let source = SELF_TYPED.replace("v -> v.length() > 1", "v -> v.$0");
    assert!(labels(&source).contains(&"length".to_string()));
    // ...and a later step of the chain stands down instead of inventing an error.
    assert_eq!(errors(SELF_TYPED), Vec::<String>::new());
}

#[test]
fn an_anonymous_class_with_a_diamond_sees_its_own_members() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.util.function.Supplier;\npublic class T {\n  static <V> Supplier<V> memo(Supplier<V> in) {\n    return new Supplier<>() {\n      private volatile boolean initialized;\n      private V value;\n      public V get() {\n        if (!initialized) {\n          value = in.get();\n          initialized = true;\n        }\n        return value;\n      }\n    };\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn an_anonymous_class_in_a_static_method_sees_its_own_members() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.util.function.Supplier;\npublic class T {\n  static <V> Supplier<V> memo(Supplier<V> in) {\n    return new Supplier<V>() {\n      private boolean initialized;\n      public V get() {\n        if (!initialized) { initialized = true; }\n        return null;\n      }\n    };\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn member_types_inherited_from_an_implemented_interface_resolve_by_simple_name() {
    if !has_stdlib() {
        return;
    }
    let source = "interface Watcher {\n  interface Request { }\n  Request directory(String root);\n}\npublic class T implements Watcher {\n  public Request directory(String root) { return new Lazy(root); }\n  private final class Lazy implements Request {\n    Lazy(String root) { }\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn two_modules_declaring_the_same_class_each_keep_their_own_members() {
    if !has_stdlib() {
        return;
    }
    // Test classes of different modules often share a qualified name; each file means its own.
    let first = "package p;\npublic class SameTest {\n  private static String alpha = \"a\";\n  static void run() { alpha = alpha + 1; }\n}\n";
    let second = "package p;\npublic class SameTest {\n  private static String beta = \"b\";\n  static void run() { beta = beta + 1; }\n}\n";
    let fixture = Fixture::with_config(
        &[("a/p/SameTest.java", first), ("b/p/SameTest.java", second)],
        config(),
    );
    for name in ["a/p/SameTest.java", "b/p/SameTest.java"] {
        let file = fixture.file(name);
        let found: Vec<String> = diagnostics::diagnostics(&fixture.db, file)
            .iter()
            .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
            .map(diagnostics::describe)
            .collect();
        assert!(found.is_empty(), "{name}: {found:?}");
    }
}

#[test]
fn unqualified_calls_in_a_duplicated_class_resolve_to_that_files_methods() {
    if !has_stdlib() {
        return;
    }
    let first = "package p;\npublic class SameTest {\n  static void send(String s) {}\n  static void run() { send(\"x\"); }\n}\n";
    let second = "package p;\npublic class SameTest {\n  static void deliver(int n) {}\n  static void run() { deliver(1); }\n}\n";
    let fixture = Fixture::with_config(
        &[("a/p/SameTest.java", first), ("b/p/SameTest.java", second)],
        config(),
    );
    for name in ["a/p/SameTest.java", "b/p/SameTest.java"] {
        let file = fixture.file(name);
        let found: Vec<String> = diagnostics::diagnostics(&fixture.db, file)
            .iter()
            .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
            .map(diagnostics::describe)
            .collect();
        assert!(found.is_empty(), "{name}: {found:?}");
    }
}

#[test]
fn an_inner_class_uses_its_outer_classs_type_variable_as_the_same_variable() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.util.*;\npublic class T<K extends Enum<K>, V> extends AbstractMap<K, V> {\n  private final K[] universe;\n  T(K[] universe) { this.universe = universe; }\n  public Set<Entry<K, V>> entrySet() { return null; }\n  private final class KeySet extends AbstractSet<K> {\n    K first() { return universe[0]; }\n    public Iterator<K> iterator() { return null; }\n    public int size() { return universe.length; }\n  }\n  KeySet keys() { return new KeySet(); }\n  K take(int i) { K k = keys().first(); return k; }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn cloning_an_array_keeps_its_type() {
    if !has_stdlib() {
        return;
    }
    let source = "import java.lang.reflect.Type;\npublic class T {\n  private final Type[] types = new Type[0];\n  Type[] copy() {\n    Type[] a = types.clone();\n    int[] b = new int[2].clone();\n    return types.clone();\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn a_covariant_return_declared_through_another_interface_wins() {
    if !has_stdlib() {
        return;
    }
    let source = "public class T {\n  interface Headers { String get(String n); }\n  interface MutableHeaders extends Headers { MutableHeaders add(String n, String v); }\n  interface Message { Headers getHeaders(); }\n  interface MutableMessage extends Message { MutableHeaders getHeaders(); }\n  interface Response extends Message { }\n  interface MutableResponse extends Response, MutableMessage { }\n  void m(MutableResponse r) {\n    r.getHeaders().add(\"a\", \"b\");\n    MutableHeaders h = r.getHeaders();\n  }\n}\n";
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn constant_int_narrows_to_byte_short_char_without_a_cast() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    static final int LIMIT = 5;
    byte version() { return 3; }
    char letter() { return 'a' + 1; }
    void f(int n) {
        byte a = 0;
        byte b = -1;
        byte c = 0x7f;
        short d = 300;
        char e = 65;
        Byte boxed = 5;
        byte g = LIMIT;
        byte h = (byte) 200;
        int octal = (n & 0_700) >> 6;
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn constants_that_do_not_fit_or_are_not_constant_still_fail_to_narrow() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    int value() { return 1; }
    void f() {
        byte a = 200;
        char b = -1;
        byte c = value();
    }
    byte g() { return 128; }
}
"#;
    assert_eq!(errors(source).len(), 4, "{:?}", errors(source));
}

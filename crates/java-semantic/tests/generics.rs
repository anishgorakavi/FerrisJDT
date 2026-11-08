//! Generic method inference: type variables of a method, wildcards, varargs,
//! lambdas and method references that determine a type, and nested calls whose
//! type comes from the call around them.
//!
//! Every case here is code `javac` accepts, so each expected type is what a
//! student would see in hover and completion.

use java_semantic::fixture::Fixture;
use java_semantic::infer::Inferer;
use java_semantic::scope::context_at;
use java_syntax::ast;

/// Declarations shared by the cases: generic helpers of the shapes that appear
/// in coursework, with the bounds written the way textbooks write them.
const HELPERS: &str = r#"
  static <T> T id(T t) { return t; }
  static <T> T pick(List<? extends T> l) { return l.get(0); }
  static <T extends Comparable<? super T>> T maxOf(List<? extends T> items) { return items.get(0); }
  static <E extends Enum<E>> E first(Class<E> c) { return c.getEnumConstants()[0]; }
  static <T extends Number & Comparable<T>> T clamp(T v, T lo, T hi) { return v; }
  static <T> void sortDesc(List<T> l, Comparator<? super T> c) { }
  static <A, B> Pair<A, B> pair(A a, B b) { return new Pair<>(a, b); }
  static class Pair<A, B> {
    A a; B b;
    Pair(A a, B b) { this.a = a; this.b = b; }
    A first() { return a; }
    B second() { return b; }
    <C> Pair<A, C> withSecond(C c) { return new Pair<>(a, c); }
  }
  static class Box<T> {
    T v;
    Box(T v) { this.v = v; }
    T get() { return v; }
    <R> Box<R> map(Function<? super T, ? extends R> f) { return new Box<>(f.apply(v)); }
  }
"#;

/// The type of the expression ending at `$0`, inside a method with `ints`,
/// `strs` and `names` in scope.
fn type_in(body: &str) -> String {
    let source = format!(
        "import java.util.*;\nimport java.util.concurrent.TimeUnit;\nimport java.util.function.*;\nimport java.util.stream.*;\n\
         public class T {{\n{HELPERS}\n  void m(List<Integer> ints, List<String> strs, Map<String, List<Integer>> names) {{\n{body}\n  }}\n}}\n"
    );
    let fixture = Fixture::new(&[("T.java", &source)]);
    if !fixture.has_stdlib() {
        return "skipped".to_string();
    }
    let (file, offset) = fixture.cursor();
    let context = context_at(&fixture.db, file, offset).expect("a context at the cursor");
    let inferer = Inferer::new(&context);
    match ast::expression_ending_at(context.source.tree(), offset) {
        Some(expression) => inferer.type_of(expression).display_short(),
        None => "<no expression>".to_string(),
    }
}

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        let got = type_in(body);
        assert!(
            got == "skipped" || got == *expected,
            "for `{body}`: expected {expected}, got {got}"
        );
    }
}

#[test]
fn a_methods_own_type_variable_is_solved_from_its_arguments() {
    check(&[
        ("id(\"x\")$0;", "String"),
        ("id(5)$0;", "Integer"),
        ("pair(1, \"a\")$0;", "Pair<Integer, String>"),
        ("pair(1, \"a\").second()$0;", "String"),
        (
            "new Pair<>(1, \"a\").withSecond(2.5)$0;",
            "Pair<Integer, Double>",
        ),
        ("clamp(5, 1, 9)$0;", "Integer"),
    ]);
}

#[test]
fn varargs_arguments_bind_the_element_type() {
    check(&[
        ("Arrays.asList(1)$0;", "List<Integer>"),
        ("Arrays.asList(3, 1, 2)$0;", "List<Integer>"),
        ("Arrays.asList(\"a\", \"b\")$0;", "List<String>"),
        // Differing element types meet at their closest common superclass.
        ("Arrays.asList(1, 2.5)$0;", "List<Number>"),
        ("Arrays.asList(1, 2).get(0)$0;", "Integer"),
    ]);
}

#[test]
fn a_static_method_is_not_confused_with_its_classs_type_parameter() {
    // `Optional<T>` and `Stream<T>` each declare a static `<T> of(T)`.
    check(&[
        ("Optional.of(\"x\")$0;", "Optional<String>"),
        ("Stream.of(\"a\")$0;", "Stream<String>"),
        ("Stream.of(\"a\", \"b\")$0;", "Stream<String>"),
        ("Stream.of(1, 2).map(n -> n)$0;", "Stream<Integer>"),
    ]);
}

#[test]
fn wildcard_parameters_bind_through_the_argument() {
    check(&[
        ("pick(ints)$0;", "Integer"),
        ("maxOf(ints)$0;", "Integer"),
        ("maxOf(strs)$0;", "String"),
        ("Collections.max(ints)$0;", "Integer"),
        ("Collections.min(strs)$0;", "String"),
        // A bound that mentions the variable itself must not hide the argument.
        ("first(TimeUnit.class)$0;", "TimeUnit"),
    ]);
}

#[test]
fn a_methods_type_parameter_bounds_are_visible_in_its_body() {
    let bounded = "import java.util.*;\npublic class T {\n\
        static <T extends Comparable<T>> int a(T item) { return item.compareTo(item)$0; }\n}";
    let loop_var = "import java.util.*;\npublic class T {\n\
        static <T extends Comparable<? super T>> int b(List<? extends T> items) {\n\
        int r = 0; for (T item : items) { r = item.compareTo(item)$0; } return r; }\n}";
    let local = "import java.util.*;\npublic class T {\n\
        static <T extends Comparable<? super T>> int c(List<? extends T> items) {\n\
        T first = items.get(0); return first.compareTo(first)$0; }\n}";
    let intersection = "public class T {\n\
        static <T extends Number & Comparable<T>> int d(T v, T lo) { return v.intValue()$0 + v.compareTo(lo); }\n}";
    let second_bound = "public class T {\n\
        static <T extends Number & Comparable<T>> int e(T v, T lo) { return v.compareTo(lo)$0; }\n}";
    for (source, expected) in [
        (bounded, "int"),
        (loop_var, "int"),
        (local, "int"),
        (intersection, "int"),
        (second_bound, "int"),
    ] {
        let fixture = Fixture::new(&[("T.java", source)]);
        if !fixture.has_stdlib() {
            return;
        }
        let (file, offset) = fixture.cursor();
        let context = context_at(&fixture.db, file, offset).unwrap();
        let inferer = Inferer::new(&context);
        let expression = ast::expression_ending_at(context.source.tree(), offset).unwrap();
        assert_eq!(
            inferer.type_of(expression).display_short(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn a_lambda_body_determines_the_type_it_returns() {
    check(&[
        (
            "Optional.of(\"x\").map(s -> s.length())$0;",
            "Optional<Integer>",
        ),
        (
            "Stream.of(\"a\").map(s -> s.length())$0;",
            "Stream<Integer>",
        ),
        ("strs.stream().map(s -> s.length())$0;", "Stream<Integer>"),
        (
            "strs.stream().map(s -> { return s.trim(); })$0;",
            "Stream<String>",
        ),
        (
            "new Box<>(\"s\").map(s -> s.length() * 2.0)$0;",
            "Box<Double>",
        ),
    ]);
}

#[test]
fn a_method_reference_determines_the_type_it_returns() {
    check(&[
        ("new Box<>(\"s\").map(String::length)$0;", "Box<Integer>"),
        ("ints.stream().map(String::valueOf)$0;", "Stream<String>"),
        ("strs.stream().map(String::trim)$0;", "Stream<String>"),
        (
            "strs.stream().map(StringBuilder::new)$0;",
            "Stream<StringBuilder>",
        ),
        (
            "new Box<>(\"s\").map(String::length).map(n -> n * 2.0).get()$0;",
            "Double",
        ),
    ]);
}

#[test]
fn a_diamond_takes_its_arguments_from_the_constructor() {
    check(&[
        ("new Box<>(\"s\")$0;", "Box<String>"),
        ("new Pair<>(1, \"a\")$0;", "Pair<Integer, String>"),
    ]);
}

#[test]
fn a_nested_generic_call_takes_its_type_from_the_call_around_it() {
    check(&[
        ("ints.stream().collect(Collectors.toList())$0;", "List<Integer>"),
        ("ints.stream().map(String::valueOf).collect(Collectors.toList())$0;", "List<String>"),
        ("strs.stream().map(s -> s.length()).collect(Collectors.toList()).get(0)$0;", "Integer"),
        ("ints.stream().collect(Collectors.toSet())$0;", "Set<Integer>"),
        (
            "ints.stream().collect(Collectors.groupingBy(i -> i % 2))$0;",
            "Map<Integer, List<Integer>>",
        ),
        (
            "ints.stream().collect(Collectors.groupingBy(Integer::intValue))$0;",
            "Map<Integer, List<Integer>>",
        ),
        (
            "strs.stream().collect(Collectors.groupingBy(String::length))$0;",
            "Map<Integer, List<String>>",
        ),
        (
            "strs.stream().collect(Collectors.partitioningBy(s -> s.length() > 1))$0;",
            "Map<Boolean, List<String>>",
        ),
        (
            "ints.stream().collect(Collectors.toMap(i -> i, i -> \"v\"))$0;",
            "Map<Integer, String>",
        ),
        (
            "strs.stream().collect(Collectors.groupingBy(String::length, Collectors.counting()))$0;",
            "Map<Integer, Long>",
        ),
    ]);
}

#[test]
fn a_lambda_parameter_takes_its_type_from_a_solved_type_variable() {
    // The cursor sits inside the lambda body, after the parameter.
    check(&[
        ("sortDesc(strs, (a, b) -> b$0.isEmpty());", "String"),
        ("Collections.sort(ints, (a, b) -> b.compareTo(a)$0 + 1);", "int"),
        ("strs.stream().map(s -> s$0.trim());", "String"),
        ("Optional.of(\"x\").map(s -> s$0.trim());", "String"),
        ("ints.stream().collect(Collectors.groupingBy(i -> i$0 % 2));", "Integer"),
        // The second lambda's parameter comes from the comparator the first built.
        (
            "List<Map.Entry<String, Integer>> es = null; es.sort(Comparator.comparing((Map.Entry<String, Integer> e) -> e.getValue()).thenComparing(e -> e$0.getKey()));",
            "Entry<String, Integer>",
        ),
    ]);
}

#[test]
fn generics_nested_several_levels_deep_keep_their_arguments() {
    check(&[
        ("names.get(\"a\").get(0)$0;", "Integer"),
        (
            "Map<String, List<Map<Integer, Set<String>>>> m = null; m.get(\"a\").get(0).get(1)$0;",
            "Set<String>",
        ),
        (
            "Pair<Pair<String, List<Integer>>, Box<Box<Double>>> p = null; p.second().get().get()$0;",
            "Double",
        ),
        (
            "Pair<Pair<String, List<Integer>>, Box<Box<Double>>> p = null; p.first().second().get(0)$0;",
            "Integer",
        ),
    ]);
}

#[test]
fn an_unsolvable_variable_stays_unknown_rather_than_object() {
    // Nothing fixes `T` here, so claiming `Object` would invite false errors.
    check(&[("Collections.emptyList()$0;", "List<<unknown>>")]);
}

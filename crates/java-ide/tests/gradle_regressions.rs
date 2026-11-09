//! Valid Java found while checking the Gradle build tool's own sources that used to be flagged.

use java_ide::diagnostics;
use java_semantic::fixture::Fixture;

fn errors(source: &str) -> Vec<String> {
    let fixture = Fixture::new(&[("T.java", source)]);
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

#[test]
fn expected_type_does_not_override_a_bounded_type_variable() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    static <X> X cast(Object o) { return null; }
    static <E extends Enum<E>> E toEnum(Class<E> c, Object v) { return null; }
    static <E extends Number> E num(E e) { return null; }
    Object f(Class<?> type, Object v) {
        Object a = toEnum(cast(type), v);
        Object b = num(cast(type));
        return toEnum(cast(type), v);
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn type_variable_bounded_by_another_variable_converts_to_it() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
abstract class T<E> {
    abstract boolean add(E e);
    abstract Collection<E> store();
    protected <I extends E> boolean doAdd(I item) {
        if (store().add(item)) { add(item); }
        E same = item;
        return add(item);
    }
    <U extends E> E pick(U u) { return u; }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn qualified_this_keeps_the_outer_type_arguments() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
abstract class T<K, V> {
    abstract V base(K key);
    class Inner {
        V get(K key) { V w = T.this.base(key); return T.this.base(key); }
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn anonymous_class_inherits_members_with_the_written_type_arguments() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
class T {
    Object f() {
        return new ArrayList<String>() {
            String first() { return get(0); }
        };
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn get_class_is_class_extends_the_static_type() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    T make() throws Exception {
        return getClass().getConstructor().newInstance();
    }
    T cast(Object o) { return getClass().cast(o); }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn lambda_body_shape_picks_the_value_or_void_overload() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
interface Action<A> { void execute(A a); }
interface Reader<A, R> { R read(A a); }
class Result<V> { V get() { return null; } }
class T {
    Result<Void> with(Action<String> action) { return null; }
    <R> Result<R> with(Reader<String, ? extends R> reader) { return null; }
    String value() { String s = with(x -> { return x.trim(); }).get(); return s; }
    void effect() { Result<Void> r = with(x -> { System.out.println(x); }); }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn partially_known_inference_result_is_kept_instead_of_the_declared_bound() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
import java.util.stream.*;
class T {
    static <E> ArrayDeque<E> create(int size) { return new ArrayDeque<>(size); }
    ArrayDeque<String> tail(Stream<String> lines) {
        ArrayDeque<String> kept = lines.collect(Collectors.toCollection(() -> create(3)));
        return kept;
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn unqualified_call_in_a_nested_class_uses_the_inherited_method_not_the_outer_one() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
abstract class T<V> {
    abstract V value();
    static abstract class Success<V> extends T<V> {
        V again() { return value(); }
    }
    static abstract class Other<W> extends T<W> {
        W again() { W w = value(); return w; }
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn generic_call_passed_where_a_capture_is_expected_is_not_guessed() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
class T<V> {
    interface Box<X> { X orElse(X other); }
    static <U> U cast(Object o) { return null; }
    V f(Box<? extends V> box, V fallback) {
        return box.orElse(cast(fallback));
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

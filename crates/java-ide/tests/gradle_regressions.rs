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

#[test]
fn volatile_protected_field_of_a_jdk_class_is_visible() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.io.*;
class T extends FilterInputStream {
    T(InputStream i) throws IOException {
        super(new BufferedInputStream(i));
        this.in.mark(3);
        int b = in.read();
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn expected_iterable_does_not_force_a_collection_bounded_collector_result() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
import java.util.stream.*;
class T {
    Iterable<String> rel(Collection<java.io.File> files) {
        return files.stream().map(f -> "lib/" + f.getName()).collect(Collectors.toCollection(ArrayList::new));
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn super_wildcard_parameter_does_not_widen_an_already_fixed_variable() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    interface Action<A> { void execute(A a); }
    interface Repo {}
    private final Action<Repo> addLast = r -> {};
    <R extends Repo> R add(R r) { return addWith(r, addLast); }
    private <R extends Repo> R addWith(R r, Action<? super R> a) { return r; }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn lambda_argument_skips_array_and_class_parameters() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
class T {
    interface Action<A> { void execute(A a); }
    interface Op { Op sign(String... f); }
    interface Task { void a(); void b(); }
    interface Pub { String name(); }
    static class Closure<A> {}
    List<String> sign(Task... t) { return null; }
    List<String> sign(Pub... p) { return null; }
    Op sign(String... f) { return sign(op -> op.sign(f)); }
    Op sign(Closure<?> c) { return null; }
    Op sign(Action<Op> setup) { return null; }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn qualified_name_finds_a_member_type_inherited_from_a_superclass() {
    let source = r#"
class Base { enum Attribute { A; static Attribute from(int i) { return A; } } }
class Sub extends Base {}
class T {
    String f() { return Sub.Attribute.from(1).name() + Sub.Attribute.A; }
    Sub.Attribute g() { return Sub.Attribute.A; }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn covariant_override_wins_when_a_diamond_reaches_the_original_again() {
    let source = r#"
class T {
    interface Action<A> { void execute(A a); }
    interface Soft {}
    interface Bin extends Soft { String name(); }
    interface Coll<A> { void finalized(Action<? super A> a); }
    interface WithBins extends Soft { Coll<? extends Soft> getBinaries(); }
    interface Prod extends Soft {}
    interface CppComp extends WithBins, Prod { Coll<? extends Bin> getBinaries(); }
    interface ProdCpp extends CppComp, Prod {}
    boolean tested(Bin b) { return b.name() != null; }
    void run(ProdCpp p) {
        p.getBinaries().finalized(b -> { if (!tested(b)) { return; } });
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn inner_class_extending_an_inherited_inner_class_gets_the_outer_type_arguments() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
abstract class Base<A> {
    protected class It implements Iterator<A> {
        final List<A> backing = null;
        A next;
        public boolean hasNext() { return false; }
        public A next() { return next; }
    }
}
class T<A> extends Base<A> {
    class SubIt extends It {
        A previous;
        public A next() {
            A value = super.next();
            previous = backing.get(0);
            previous = next;
            return value;
        }
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn qualified_super_method_reference_is_not_an_unknown_type() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.function.*;
class P { void add(String s) {} }
class T extends P {
    final Consumer<String> addLast = T.super::add;
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn argument_type_replaces_the_expected_type_a_candidate_overload_suggested() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
import java.util.stream.*;
class T {
    interface PSet<K> extends Iterable<K> {
        static <K> Collector<K, Object, PSet<K>> toPSet() { return null; }
    }
    interface Spec {}
    interface Meta {}
    interface Edge { Spec ex(); }
    static class Excl {
        Spec any(Collection<? extends Meta> m) { return null; }
        Spec any(PSet<Spec> s) { return null; }
    }
    Spec f(Excl e, List<Edge> edges) {
        return e.any(edges.stream().map(Edge::ex).filter(Objects::nonNull).collect(PSet.toPSet()));
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn class_nested_in_a_generic_interface_has_no_enclosing_instance_and_keeps_its_own_variables() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.function.Function;
interface Reader<A> {
    class Ctx { <A> A compute(String key, Function<String, A> f) { return f.apply(key); } }
}
class T implements Reader<Number> {
    String m(Ctx c) { return c.compute("k", k -> "x"); }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn anonymous_class_with_a_diamond_base_leaves_its_type_argument_unknown() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
class T {
    static abstract class AbsIt<A> implements Iterator<A> {
        protected final A endOfData() { return null; }
        protected abstract A computeNext();
        public boolean hasNext() { return false; }
        public A next() { return computeNext(); }
    }
    Iterable<String> anc(int n) {
        return () -> new AbsIt<>() {
            protected String computeNext() {
                if (n == 0) { return endOfData(); }
                return "x";
            }
        };
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn variable_declared_under_one_case_is_visible_in_the_next() {
    let source = r#"
class T {
    String f(int k) {
        switch (k) {
            case 1:
                String reason = "a";
                return reason;
            case 2:
                reason = "b";
                return reason;
            default:
                return "";
        }
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn enum_constant_body_sees_its_own_private_methods_and_the_enum_through_super() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
enum T {
    GETTER,
    SETTER {
        @Override
        Class<?> typeFor(String m) { requireSingle(m); return String.class; }
        private void requireSingle(String m) { }
        @Override
        Map<String, String> defs() { Map<String, String> d = super.defs(); d.put("a", "b"); return d; }
    };
    Class<?> typeFor(String m) { return Object.class; }
    Map<String, String> defs() { return new HashMap<>(); }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn lone_call_lambda_prefers_the_value_returning_overload() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.function.Supplier;
class T {
    interface Factory<A> { A create(); }
    interface Sync { void withLock(Runnable r); <A> A withLock(Factory<A> f); }
    static abstract class Res<A> { abstract A value(); static <A> Res<A> ok(A v) { return null; } }
    private <S> Res<S> doIt(Supplier<Res<S>> action) { return null; }
    <S> S transition(Sync sync, Supplier<? extends S> action) {
        return sync.withLock(() -> doIt(() -> Res.ok(action.get())).value());
    }
    void run(Sync sync, Runnable r) {
        sync.withLock(() -> r.run());
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn super_wildcard_argument_does_not_name_a_variable_the_context_can() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
import java.util.*;
class T {
    interface Described { } 
    interface Located { }
    interface Annotated { }
    static class Pred<A> { }
    static class Cond<A> { }
    static class JavaClass implements Described, Located, Annotated { }
    static <A extends Described & Located> Cond<A> be(Pred<? super A> p) { return null; }
    static Pred<Annotated> annotatedWith() { return null; }
    static void should(Cond<? super JavaClass> c) { }
    void f() { should(be(annotatedWith())); }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn classic_switch_statement_is_accepted_at_java_8_but_arrow_and_value_switches_are_not() {
    let classic = r#"
class T {
    int f(int k) { switch (k) { case 1: return 1; default: return 2; } }
}
"#;
    assert_eq!(errors(classic), Vec::<String>::new());
    let arrow = "class T { void f(int k) { switch (k) { case 1 -> {} default -> {} } } }";
    let value = "class T { int f(int k) { int v = switch (k) { case 1: yield 2; default: yield 3; }; return v; } }";
    for source in [arrow, value] {
        let found = errors(source);
        assert!(
            found.iter().any(|e| e.contains("require source level 14")),
            "{source}: {found:?}"
        );
    }
}

#[test]
fn super_wildcard_argument_of_a_wildcard_type_names_the_call_variable_from_the_argument() {
    let source = r#"
class T<A> {
    interface Action<X> { void execute(X x); }
    <I extends A> Action<? super I> wrap(Action<? super I> a) { return a; }
    class Prov<I extends A> {
        I get() { return null; }
        void configure(Action<? super I> action) { wrap(action).execute(get()); }
    }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

#[test]
fn numeric_ternary_with_a_fitting_constant_has_the_narrow_type() {
    if !has_stdlib() {
        return;
    }
    let source = r#"
class T {
    short s(Short n) { return n == null ? 0 : n; }
    char c(Character n) { return n == null ? 0 : n; }
    byte b(Byte n, boolean f) { return f ? n : 1; }
    long l(int a, long b, boolean f) { return f ? a : b; }
    int i(Integer a, boolean f) { return f ? a : 0; }
}
"#;
    assert_eq!(errors(source), Vec::<String>::new());
}

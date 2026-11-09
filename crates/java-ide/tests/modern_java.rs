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

//! Tests for parsing, incremental edits, and error recovery.

use java_syntax::line_index::Position;
use java_syntax::{ast, SourceFile};

#[test]
fn parses_a_class_and_finds_declarations() {
    let src = "package ds;\npublic class Stack<E> {\n    private E[] data;\n    public void push(E e) {}\n}\n";
    let file = SourceFile::new(src.to_string(), 1);
    assert!(
        file.syntax_errors().is_empty(),
        "{:?}",
        file.syntax_errors()
    );
    let root = file.tree().root_node();
    let class = ast::named_children(root)
        .find(|n| n.kind() == "class_declaration")
        .expect("class found");
    assert_eq!(ast::name_text(class, file.text()), Some("Stack"));
}

#[test]
fn reports_syntax_errors_with_ranges() {
    let file = SourceFile::new("class A { void f() { int = ; } }".to_string(), 1);
    let errors = file.syntax_errors();
    assert!(!errors.is_empty());
    for error in &errors {
        assert!(error.end > error.start, "error ranges must be non-empty");
        assert!(error.end <= file.text().len() as u32);
    }
}

#[test]
fn tolerates_truncated_declarations() {
    // The prompt's explicit requirement: this must not crash and must still
    // yield a tree with the class in it.
    let file = SourceFile::new("public void add(".to_string(), 1);
    assert!(!file.syntax_errors().is_empty());
    let file = SourceFile::new("public class A {\n  public void add(\n}".to_string(), 1);
    assert!(!file.syntax_errors().is_empty());
    let class =
        ast::named_children(file.tree().root_node()).find(|n| n.kind() == "class_declaration");
    assert!(class.is_some(), "the class survives an unterminated method");
}

#[test]
fn never_panics_on_adversarial_input() {
    let cases = [
        "",
        "\0\0\0",
        "class",
        "class {{{{{{{{{{",
        "}}}}}}",
        "/*",
        "\"unterminated",
        "class A { A<A<A<A<A<A<A<A<A<A<A<A<A<A>>>>>>>>>>>>> x; }",
        "\u{1F980}\u{1F980}",
        "class A { void f() { ((((((((((1)))))))))); } }",
    ];
    for case in cases {
        let file = SourceFile::new(case.to_string(), 1);
        let _ = file.syntax_errors();
        let _ = ast::named_node_at(file.tree(), 0);
        let _ = file.position_at(case.len() as u32);
    }
    // Deep nesting must not blow the stack while collecting errors.
    let deep = format!("class A {{ void f() {{ {} }} }}", "if (x) {".repeat(400));
    let file = SourceFile::new(deep, 1);
    let _ = file.syntax_errors();
}

#[test]
fn incremental_edit_matches_a_full_reparse() {
    let original = "class A {\n    int x;\n    void f() { int y = 1; }\n}\n";
    let mut file = SourceFile::new(original.to_string(), 1);
    // Type `yz` over `y` on line 2.
    file.apply_change(Position::new(2, 19), Position::new(2, 20), "longer", 2);
    let expected = original.replacen("int y =", "int longer =", 1);
    assert_eq!(file.text(), expected);
    assert_eq!(file.version(), 2);
    let fresh = SourceFile::new(expected.clone(), 2);
    assert_eq!(
        ast::debug_tree(file.tree().root_node(), file.text()),
        ast::debug_tree(fresh.tree().root_node(), fresh.text()),
        "incremental reparse must agree with a fresh parse"
    );
}

#[test]
fn incremental_edits_survive_a_long_typing_session() {
    // Types a method body one character at a time, the way an editor sends
    // `didChange`, and checks the tree against a fresh parse at every step.
    let mut file = SourceFile::new("class A {\n}\n".to_string(), 1);
    let typed = "    void f(int n) { for (int i = 0; i < n; i++) { System.out.println(i); } }\n";
    let mut version = 1;
    for (index, ch) in typed.char_indices() {
        let line = 1u32;
        let character = index as u32;
        version += 1;
        file.apply_change(
            Position::new(line, character),
            Position::new(line, character),
            &ch.to_string(),
            version,
        );
        let fresh = SourceFile::new(file.text().to_string(), version);
        assert_eq!(
            ast::debug_tree(file.tree().root_node(), file.text()),
            ast::debug_tree(fresh.tree().root_node(), fresh.text()),
            "divergence after typing {:?}",
            &typed[..index + ch.len_utf8()]
        );
    }
    assert!(file.syntax_errors().is_empty());
}

#[test]
fn deleting_a_range_updates_text_and_tree() {
    let mut file = SourceFile::new("class A { void f() {} void g() {} }".to_string(), 1);
    file.apply_change(Position::new(0, 10), Position::new(0, 22), "", 2);
    assert_eq!(file.text(), "class A { void g() {} }");
    assert!(file.syntax_errors().is_empty());
}

#[test]
fn identifier_at_resolves_either_side_of_the_cursor() {
    let src = "class A { void f() { value.compareTo(x); } }";
    let file = SourceFile::new(src.to_string(), 1);
    let at_value = src.find("value").unwrap() as u32 + 2;
    let node = ast::identifier_at(file.tree(), at_value).expect("inside an identifier");
    assert_eq!(ast::text(node, file.text()), "value");
    // Just past the end of `compareTo` still resolves it, which is where the
    // cursor sits when a request arrives mid-word.
    let after = src.find("compareTo").unwrap() as u32 + "compareTo".len() as u32;
    let node = ast::identifier_at(file.tree(), after).expect("at the end of an identifier");
    assert_eq!(ast::text(node, file.text()), "compareTo");
}

#[test]
fn type_use_annotations_before_a_varargs_ellipsis_parse_cleanly() {
    for parameter in [
        "String @Nullable ... names",
        "String @Nullable... names",
        "Class<? extends Annotation> @org.jspecify.annotations.Nullable ... annotations",
        "int @A @B(\"x\") ... values",
    ] {
        let source = format!("class A {{ void f({parameter}) {{}} }}");
        let file = SourceFile::new(source.clone(), 1);
        assert!(
            file.syntax_errors().is_empty(),
            "{parameter}: {:?}",
            file.syntax_errors()
        );
        // Offsets are those of the real text: the declaration is still found where it is written.
        assert_eq!(file.text(), source);
        let root = file.tree().root_node();
        assert_eq!(root.end_byte(), source.len());
    }
}

#[test]
fn an_ordinary_ellipsis_is_left_alone_and_real_errors_still_show() {
    let ok = SourceFile::new("class A { void f(String... names) {} }".to_string(), 1);
    assert!(ok.syntax_errors().is_empty());
    let broken = SourceFile::new("class A { void f(String @Nullable ... ) {} ".to_string(), 1);
    assert!(!broken.syntax_errors().is_empty());
}

#[test]
fn editing_a_file_with_masked_annotations_matches_a_fresh_parse() {
    use java_syntax::Position;
    let mut file = SourceFile::new(
        "class A {\n  void f(String @Nullable ... names) {}\n  void g() {}\n}\n".to_string(),
        1,
    );
    // Type inside g(), then break the masked parameter, then repair it.
    file.apply_change(
        Position::new(2, 12),
        Position::new(2, 12),
        " int x = 1; ",
        2,
    );
    assert!(file.syntax_errors().is_empty());
    file.apply_change(Position::new(1, 19), Position::new(1, 28), "", 3);
    let fresh = SourceFile::new(file.text().to_string(), 3);
    assert_eq!(file.syntax_errors().len(), fresh.syntax_errors().len());
    assert_eq!(
        file.tree().root_node().to_sexp(),
        fresh.tree().root_node().to_sexp()
    );
}

#[test]
fn octal_literal_with_underscore_after_the_leading_zero_parses() {
    let src = "class A { int f(int mode) { return (mode & 0_700) >> 6; } int g = 0__17; String s = \"0_9\"; }";
    let file = SourceFile::new(src.to_string(), 1);
    assert!(
        file.syntax_errors().is_empty(),
        "{:?}",
        file.syntax_errors()
    );
    assert_eq!(file.text(), src, "masking must not change the stored text");
}

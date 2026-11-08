//! Tests for each IDE feature, asserting on what the editor would actually
//! show. A feature with no test here is not claimed as supported.

use java_ide::completion::CompletionSettings;
use java_ide::{
    code_actions, completion, diagnostics, hover, imports, navigation, rename, signature, symbols,
    tokens,
};
use java_semantic::fixture::Fixture;
use lsp_types::{CompletionItemKind, DocumentSymbol, Position, Range};

fn settings() -> CompletionSettings {
    CompletionSettings {
        snippets: true,
        additional_edits: true,
    }
}

/// Completion labels at the cursor, through the same entry point a server
/// uses, so the parse-repair path is covered too.
fn labels(files: &[(&str, &str)]) -> Vec<String> {
    items(files).into_iter().map(|item| item.label).collect()
}

fn items(files: &[(&str, &str)]) -> Vec<lsp_types::CompletionItem> {
    let mut fixture = Fixture::new(files);
    let (file, offset) = fixture.cursor();
    completion::completions_repairing(&mut fixture.db, file, offset, settings())
}

#[test]
fn completes_members_of_a_generic_standard_library_receiver() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        eprintln!("skipping: no JDK standard library");
        return;
    }
    let source = "import java.util.*;\npublic class T {\n  void m() {\n    List<String> names = new ArrayList<>();\n    names.$0\n  }\n}";
    let found = items(&[("T.java", source)]);
    let labels: Vec<&str> = found.iter().map(|item| item.label.as_str()).collect();
    // The members the brief names explicitly.
    for expected in ["add", "get", "size", "contains", "remove", "iterator"] {
        assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
    }
    // Unrelated types must not leak in.
    assert!(!labels.contains(&"println"), "{labels:?}");
    assert!(!labels.contains(&"compareTo"), "{labels:?}");

    // Generic substitution must be visible in the detail shown to the user.
    let add = found
        .iter()
        .find(|item| {
            item.label == "add" && item.detail.as_deref().is_some_and(|d| d.contains("String"))
        })
        .expect("add(String) is offered");
    assert!(
        add.detail.as_deref().unwrap().contains("String"),
        "{:?}",
        add.detail
    );
    let get = found.iter().find(|item| item.label == "get").unwrap();
    assert!(
        get.detail.as_deref().unwrap().ends_with("String"),
        "get should return String, got {:?}",
        get.detail
    );
    assert_eq!(get.kind, Some(CompletionItemKind::METHOD));
}

#[test]
fn completes_with_a_prefix_and_filters_by_it() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let source = "import java.util.*;\npublic class T {\n  void m() {\n    List<String> names = new ArrayList<>();\n    names.ad$0\n  }\n}";
    let found = labels(&[("T.java", source)]);
    assert!(found.contains(&"add".to_string()), "{found:?}");
    assert!(found.contains(&"addAll".to_string()), "{found:?}");
    assert!(!found.contains(&"size".to_string()), "{found:?}");
}

#[test]
fn completes_locals_fields_and_methods_in_scope() {
    let source = "public class T {\n  private int count;\n  private String label;\n  void helper() {}\n  void m(boolean flag) {\n    int local = 1;\n    $0\n  }\n}";
    let found = labels(&[("T.java", source)]);
    for expected in ["count", "label", "helper", "m", "local", "flag"] {
        assert!(found.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn completion_respects_declaration_order_for_locals() {
    // `later` is declared after the cursor and is not yet in scope.
    let source =
        "public class T {\n  void m() {\n    int earlier = 1;\n    $0\n    int later = 2;\n  }\n}";
    let found = labels(&[("T.java", source)]);
    assert!(found.contains(&"earlier".to_string()));
    assert!(!found.contains(&"later".to_string()), "{found:?}");
}

#[test]
fn completion_excludes_instance_members_from_a_static_context() {
    let source = "public class T {\n  private int instanceField;\n  private static int staticField;\n  static void m() {\n    $0\n  }\n}";
    let found = labels(&[("T.java", source)]);
    assert!(found.contains(&"staticField".to_string()), "{found:?}");
    assert!(!found.contains(&"instanceField".to_string()), "{found:?}");
}

#[test]
fn completes_static_members_through_a_class_name() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let source = "public class T {\n  void m() {\n    Integer.$0\n  }\n}";
    let found = labels(&[("T.java", source)]);
    assert!(found.contains(&"parseInt".to_string()), "{found:?}");
    assert!(found.contains(&"MAX_VALUE".to_string()), "{found:?}");
    // An instance method must not be offered on the class itself.
    assert!(!found.contains(&"intValue".to_string()), "{found:?}");
}

#[test]
fn completes_inherited_members_across_files() {
    let files = [
        (
            "Base.java",
            "public class Base {\n  protected int shared;\n  public void inherited() {}\n}",
        ),
        (
            "Derived.java",
            "public class Derived extends Base {\n  void m() {\n    this.x$0\n  }\n}",
        ),
    ];
    // With a partial member name after the dot the file still parses, which is
    // the usual state when a client asks for completions.
    let found = labels(&files);
    assert!(
        found.is_empty() || found.iter().all(|l| l.starts_with('x')),
        "{found:?}"
    );

    let files = [
        (
            "Base.java",
            "public class Base {\n  protected int shared;\n  public void inherited() {}\n}",
        ),
        (
            "Derived.java",
            "public class Derived extends Base {\n  void m() {\n    this.$0\n  }\n}",
        ),
    ];
    let found = labels(&files);
    assert!(found.contains(&"shared".to_string()), "{found:?}");
    assert!(found.contains(&"inherited".to_string()), "{found:?}");
}

/// A bare `this.` makes the parser unwind the whole class. The database keeps
/// the previous complete parse for resolution, so typing it leaves completion
/// working; this test drives the edit the way an editor would.
#[test]
fn completion_survives_an_edit_that_breaks_the_parse() {
    use java_syntax::line_index::Position as SyntaxPosition;
    let fixture = Fixture::new(&[(
        "Derived.java",
        "public class Derived {\n  protected int shared;\n  public void inherited() {}\n  void m() {\n    \n  }\n}",
    )]);
    let mut fixture = fixture;
    let path = fixture.root.join("Derived.java");
    let text = std::fs::read_to_string(&path).unwrap();
    let db = &mut fixture.db;
    let file = db.open_document(&path, text, 1);
    // Type `this.` on the empty line inside m().
    db.edit_document(file, 2, |source| {
        source.apply_change(
            SyntaxPosition::new(4, 4),
            SyntaxPosition::new(4, 4),
            "this.",
            2,
        )
    });
    let offset = db.text(file).unwrap().find("this.").unwrap() as u32 + 5;
    let found: Vec<String> = completion::completions(db, file, offset, settings())
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(found.contains(&"shared".to_string()), "{found:?}");
    assert!(found.contains(&"inherited".to_string()), "{found:?}");
}

#[test]
fn completes_a_node_receiver_with_only_its_own_members() {
    let files = [(
        "Tree.java",
        "class BSTNode<E> {\n  E data;\n  BSTNode<E> left;\n  BSTNode<E> right;\n}\n\nclass Tree<E extends Comparable<? super E>> {\n  private BSTNode<E> root;\n  private int size;\n  void m() {\n    root.$0\n  }\n}",
    )];
    let found = labels(&files);
    for expected in ["data", "left", "right"] {
        assert!(
            found.contains(&expected.to_string()),
            "missing {expected}: {found:?}"
        );
    }
    // Members of the enclosing class must not appear on the node.
    assert!(!found.contains(&"root".to_string()), "{found:?}");
    assert!(!found.contains(&"size".to_string()), "{found:?}");
}

#[test]
fn completes_a_type_parameters_bound_members() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let files = [(
        "Tree.java",
        "class Tree<E extends Comparable<? super E>> {\n  void m(E value) {\n    value.$0\n  }\n}",
    )];
    let found = labels(&files);
    assert!(found.contains(&"compareTo".to_string()), "{found:?}");
}

#[test]
fn completes_types_and_offers_an_import() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let source = "public class T {\n  void m() {\n    ArrayLis$0\n  }\n}";
    let found = items(&[("T.java", source)]);
    let array_list = found
        .iter()
        .find(|item| item.label == "ArrayList")
        .expect("ArrayList offered");
    assert_eq!(array_list.detail.as_deref(), Some("java.util.ArrayList"));
    let edits = array_list
        .additional_text_edits
        .as_ref()
        .expect("an import edit comes with it");
    assert!(
        edits[0].new_text.contains("import java.util.ArrayList;"),
        "{edits:?}"
    );
}

#[test]
fn completes_inside_an_import_statement() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let found = labels(&[("T.java", "import java.util.Arr$0\npublic class T {}")]);
    assert!(found.contains(&"ArrayList".to_string()), "{found:?}");
    let found = labels(&[("T.java", "import java.u$0\npublic class T {}")]);
    assert!(found.contains(&"util".to_string()), "{found:?}");
}

#[test]
fn completes_constructors_after_new() {
    let files = [
        ("Pair.java", "public class Pair<A, B> {}"),
        (
            "T.java",
            "public class T {\n  void m() {\n    Pair<String, Integer> p = new Pai$0\n  }\n}",
        ),
    ];
    let found = items(&files);
    let pair = found
        .iter()
        .find(|item| item.label == "Pair")
        .expect("Pair offered");
    let text = match pair.text_edit.as_ref().unwrap() {
        lsp_types::CompletionTextEdit::Edit(edit) => edit.new_text.clone(),
        _ => panic!("expected a plain edit"),
    };
    // A generic class gets its diamond so the next keystroke is useful.
    assert_eq!(text, "Pair<>(");
}

#[test]
fn completion_offers_nothing_for_an_unresolvable_receiver() {
    let source = "public class T {\n  void m() {\n    Unknown u = null;\n    u.$0\n  }\n}";
    assert!(labels(&[("T.java", source)]).is_empty());
}

#[test]
fn completion_never_panics_on_partial_input() {
    let cases = [
        "public class T { void m() { $0 } }",
        "public class T { void m() { .$0 } }",
        "public class T { void m() { a..$0 } }",
        "public class T { void m( $0",
        "public class T {$0",
        "$0",
        "import $0",
        "public class T { void m() { new $0 } }",
        "public class T { void m() { x.y.z.$0 } }",
        "public class T { void m() { this.$0 } }",
        "public class T { void m() { super.$0 } }",
    ];
    for source in cases {
        let fixture = Fixture::new(&[("T.java", source)]);
        let (file, offset) = fixture.cursor();
        let _ = completion::completions(&fixture.db, file, offset, settings());
    }
}

#[test]
fn hover_shows_a_declaration_with_its_javadoc() {
    let files = [(
        "Tree.java",
        "public class Tree {\n  /**\n   * Adds a value.\n   *\n   * @param value the value to add\n   * @return true if the tree changed\n   */\n  public boolean add(String value) { return true; }\n\n  void m() {\n    add$0(\"x\");\n  }\n}",
    )];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let hover = hover::hover(&fixture.db, file, offset).expect("hover on a call");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        _ => panic!("expected markup"),
    };
    assert!(text.contains("public boolean add(String value)"), "{text}");
    assert!(text.contains("Adds a value."), "{text}");
    assert!(text.contains("`value`"), "{text}");
    assert!(text.contains("**returns**"), "{text}");
}

#[test]
fn hover_shows_a_locals_type() {
    let source =
        "public class T {\n  void m() {\n    int count = 1;\n    int other = coun$0t;\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let hover = hover::hover(&fixture.db, file, offset).expect("hover on a local");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        _ => panic!(),
    };
    assert!(text.contains("int count"), "{text}");
    assert!(text.contains("local variable"), "{text}");
}

#[test]
fn hover_shows_standard_library_signatures_with_substitution() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    let source = "import java.util.*;\npublic class T {\n  void m() {\n    List<String> names = new ArrayList<>();\n    names.ge$0t(0);\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let hover = hover::hover(&fixture.db, file, offset).expect("hover on get");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        _ => panic!(),
    };
    // The declaration is shown as declared, naming the interface it came from.
    assert!(text.contains("get(int"), "{text}");
    assert!(text.contains("java.util.List"), "{text}");
    assert!(text.contains("rt.jar") || text.contains("ct.sym"), "{text}");
}

#[test]
fn definition_jumps_across_files() {
    let files = [
        ("Node.java", "package ds;\npublic class Node {\n  public int value;\n}"),
        ("Main.java", "package ds;\npublic class Main {\n  void m() {\n    Node n = new Node();\n    int v = n.valu$0e;\n  }\n}"),
    ];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let links = navigation::definition(&fixture.db, file, offset);
    assert_eq!(links.len(), 1, "{links:?}");
    assert!(
        links[0].target_uri.as_str().ends_with("Node.java"),
        "{:?}",
        links[0].target_uri
    );
    // Line 2 (zero-based) is `public int value;`.
    assert_eq!(links[0].target_selection_range.start.line, 2);
}

#[test]
fn definition_finds_a_local_a_method_and_a_type() {
    let source = "public class T {\n  private int field;\n  void helper() {}\n  void m() {\n    int local = 1;\n    int a = loca$0l;\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let links = navigation::definition(&fixture.db, file, offset);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_selection_range.start.line, 4);

    let source = "public class T {\n  void helper() {}\n  void m() {\n    helpe$0r();\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let links = navigation::definition(&fixture.db, file, offset);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_selection_range.start.line, 1);

    let files = [
        ("Other.java", "public class Other {}"),
        (
            "T.java",
            "public class T {\n  void m() {\n    Othe$0r o = null;\n  }\n}",
        ),
    ];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let links = navigation::definition(&fixture.db, file, offset);
    assert_eq!(links.len(), 1);
    assert!(links[0].target_uri.as_str().ends_with("Other.java"));
}

#[test]
fn definition_declines_classpath_declarations() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    // There is no source to jump to, and inventing one would be worse than
    // returning nothing.
    let source = "import java.util.*;\npublic class T {\n  void m() {\n    ArrayLis$0t<String> x = null;\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    assert!(navigation::definition(&fixture.db, file, offset).is_empty());
}

#[test]
fn references_are_semantic_not_textual() {
    // Both classes declare `size`, and only one is the target.
    let files = [
        (
            "A.java",
            "public class A {\n  int size;\n  int readA() { return size; }\n}",
        ),
        (
            "B.java",
            "public class B {\n  int size;\n  int readB() { return size; }\n}",
        ),
        (
            "C.java",
            "public class C {\n  void m(A a) { int x = a.siz$0e; }\n}",
        ),
    ];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let found = navigation::references(&fixture.db, file, offset, true);
    let files_touched: Vec<&str> = found
        .iter()
        .map(|location| {
            let path = location.uri.as_str();
            path.rsplit('/').next().unwrap()
        })
        .collect();
    assert!(files_touched.contains(&"A.java"), "{files_touched:?}");
    assert!(files_touched.contains(&"C.java"), "{files_touched:?}");
    assert!(
        !files_touched.contains(&"B.java"),
        "B.size is a different field: {files_touched:?}"
    );
    // The declaration, the read in A, and the read in C.
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn references_to_a_local_stay_inside_its_method() {
    let source = "public class T {\n  void one() {\n    int value = 1;\n    int a = valu$0e + value;\n  }\n  void two() {\n    int value = 2;\n    int b = value;\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let found = navigation::references(&fixture.db, file, offset, true);
    // Declaration plus two uses in `one`, and nothing from `two`.
    assert_eq!(found.len(), 3, "{found:?}");
    for location in &found {
        assert!(location.range.start.line <= 3, "{location:?}");
    }
}

#[test]
fn references_distinguish_overloads() {
    let source = "public class T {\n  void f(int x) {}\n  void f(String x) {}\n  void m() {\n    f(1);\n    f(\"s\");\n    f$0(2);\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let found = navigation::references(&fixture.db, file, offset, true);
    // f(int): its declaration and the two int calls, not the String call.
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
#[allow(clippy::mutable_key_type)] // see rename.rs: `Uri` keys are safe here
fn rename_updates_every_file_that_refers_to_the_symbol() {
    let files = [
        ("Node.java", "package ds;\npublic class Node {\n  public int value;\n}"),
        ("Main.java", "package ds;\npublic class Main {\n  void m() {\n    Node n = new Node();\n    int v = n.value;\n    int w = n.value;\n  }\n}"),
        ("Other.java", "package ds;\npublic class Other {\n  int value = 1;\n}"),
    ];
    let fixture = Fixture::new(&files);
    let offset = fixture.at("Node.java", "value");
    let file = fixture.file("Node.java");
    let edit = rename::rename(&fixture.db, file, offset, "payload").expect("rename succeeds");
    let changes = edit.changes.expect("changes present");
    let mut total = 0;
    for (uri, edits) in &changes {
        assert!(
            !uri.as_str().ends_with("Other.java"),
            "an unrelated field named `value` must not be renamed"
        );
        total += edits.len();
        for edit in edits {
            assert_eq!(edit.new_text, "payload");
        }
    }
    assert_eq!(total, 3, "declaration plus two uses");
}

#[test]
fn rename_refuses_invalid_names_and_classpath_symbols() {
    let fixture = Fixture::new(&[("T.java", "public class T {\n  int value;\n}")]);
    let file = fixture.file("T.java");
    let offset = fixture.at("T.java", "value");
    assert_eq!(
        rename::rename(&fixture.db, file, offset, "class").unwrap_err(),
        rename::RenameError::InvalidIdentifier("class".to_string())
    );
    assert!(rename::rename(&fixture.db, file, offset, "9bad").is_err());

    if fixture.has_stdlib() {
        let source =
            "import java.util.*;\npublic class T {\n  void m() { List<String> x = null; }\n}";
        let fixture = Fixture::new(&[("T.java", source)]);
        let file = fixture.file("T.java");
        let offset = fixture.at("T.java", "List<String>");
        let error = rename::rename(&fixture.db, file, offset, "MyList").unwrap_err();
        assert!(
            matches!(error, rename::RenameError::NotInWorkspace(_)),
            "{error:?}"
        );
    }
}

#[test]
fn document_symbols_nest_members_under_their_class() {
    let source = "package ds;\npublic class Outer {\n  private int field;\n  public Outer() {}\n  public void method(int a) {}\n  static class Inner {\n    int innerField;\n  }\n  enum Color { RED }\n}";
    let fixture = Fixture::new(&[("Outer.java", source)]);
    let file = fixture.file("Outer.java");
    let found = symbols::document_symbols(&fixture.db, file);
    assert_eq!(found.len(), 1);
    let outer = &found[0];
    assert_eq!(outer.name, "Outer");
    assert_eq!(outer.kind, lsp_types::SymbolKind::CLASS);
    let children = outer.children.as_ref().expect("members");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"field"), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("method(")), "{names:?}");
    assert!(names.contains(&"Inner"), "{names:?}");
    assert!(names.contains(&"Color"), "{names:?}");

    let inner = children.iter().find(|c| c.name == "Inner").unwrap();
    let inner_names: Vec<&str> = inner
        .children
        .as_ref()
        .unwrap()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(inner_names.contains(&"innerField"), "{inner_names:?}");

    // Every range must contain its own selection range.
    fn check(symbol: &DocumentSymbol) {
        let inside = (symbol.range.start.line, symbol.range.start.character)
            <= (
                symbol.selection_range.start.line,
                symbol.selection_range.start.character,
            )
            && (
                symbol.selection_range.end.line,
                symbol.selection_range.end.character,
            ) <= (symbol.range.end.line, symbol.range.end.character);
        assert!(
            inside,
            "{} has a selection range outside its range",
            symbol.name
        );
        for child in symbol.children.iter().flatten() {
            check(child);
        }
    }
    check(outer);
}

#[test]
fn workspace_symbols_find_classes_and_members() {
    let files = [
        ("BinarySearchTree.java", "package ds;\npublic class BinarySearchTree {\n  public boolean addValue(int v) { return true; }\n}"),
        ("Other.java", "package ds;\npublic class Other {}"),
    ];
    let fixture = Fixture::new(&files);
    let found = symbols::workspace_symbols(&fixture.db, "Binary");
    assert!(
        found.iter().any(|s| s.name == "BinarySearchTree"),
        "{found:?}"
    );
    let found = symbols::workspace_symbols(&fixture.db, "addValue");
    assert!(
        found.iter().any(|s| s.name.starts_with("addValue(")),
        "{found:?}"
    );
}

#[test]
fn signature_help_reports_the_active_parameter() {
    let source = "public class T {\n  void target(int first, String second) {}\n  void m() {\n    target(1, $0);\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let help = signature::signature_help(&fixture.db, file, offset).expect("signature help");
    assert_eq!(help.active_parameter, Some(1));
    let signature = &help.signatures[help.active_signature.unwrap_or(0) as usize];
    assert!(
        signature.label.contains("target(int first, String second)"),
        "{}",
        signature.label
    );
    let parameters = signature.parameters.as_ref().unwrap();
    assert_eq!(parameters.len(), 2);
    // The offsets must actually bracket the parameter text in the label.
    match &parameters[1].label {
        lsp_types::ParameterLabel::LabelOffsets([start, end]) => {
            assert_eq!(
                &signature.label[*start as usize..*end as usize],
                "String second"
            );
        }
        other => panic!("expected offsets, got {other:?}"),
    }
}

#[test]
fn signature_help_picks_the_matching_overload() {
    let source = "public class T {\n  void f(int a) {}\n  void f(String a, String b) {}\n  void m() {\n    f(\"x\", $0);\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let (file, offset) = fixture.cursor();
    let help = signature::signature_help(&fixture.db, file, offset).unwrap();
    let active = &help.signatures[help.active_signature.unwrap() as usize];
    assert!(
        active.label.contains("String a, String b"),
        "{}",
        active.label
    );
}

#[test]
fn signature_help_works_for_constructors() {
    let files = [
        (
            "Pair.java",
            "public class Pair {\n  public Pair(int a, int b) {}\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Pair p = new Pair($0);\n  }\n}",
        ),
    ];
    let fixture = Fixture::new(&files);
    let (file, offset) = fixture.cursor();
    let help = signature::signature_help(&fixture.db, file, offset).expect("constructor help");
    assert!(
        help.signatures[0].label.contains("Pair(int a, int b)"),
        "{:?}",
        help.signatures[0].label
    );
}

#[test]
fn diagnostics_report_syntax_errors_without_crashing() {
    let fixture = Fixture::new(&[("T.java", "public class T {\n  public void add(\n}")]);
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    assert!(!found.is_empty());
    assert!(found
        .iter()
        .any(|d| diagnostics::code_of(d) == Some(diagnostics::codes::SYNTAX)));
}

#[test]
fn diagnostics_stay_silent_on_correct_code() {
    let files = [(
        "T.java",
        "import java.util.ArrayList;\nimport java.util.List;\n\npublic class T {\n  private final List<String> items = new ArrayList<String>();\n\n  public void add(String value) {\n    items.add(value);\n  }\n\n  public int size() {\n    return items.size();\n  }\n}",
    )];
    let fixture = Fixture::new(&files);
    if !fixture.has_stdlib() {
        return;
    }
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let errors: Vec<String> = found
        .iter()
        .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
        .map(diagnostics::describe)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn diagnostics_stay_silent_on_valid_generic_code() {
    let source = r#"import java.util.*;
import java.util.concurrent.TimeUnit;
import java.util.function.*;
import java.util.stream.*;

public class T {
    static <T> T id(T t) { return t; }
    static <T extends Comparable<? super T>> T maxOf(List<? extends T> items) {
        T best = items.get(0);
        for (T item : items) {
            if (item.compareTo(best) > 0) { best = item; }
        }
        return best;
    }
    static <E extends Enum<E>> E first(Class<E> c) { return c.getEnumConstants()[0]; }
    static <T extends Number & Comparable<T>> T clamp(T v, T lo, T hi) {
        return v.compareTo(lo) < 0 ? lo : v;
    }
    static <T> void sortDesc(List<T> l, Comparator<? super T> c) { l.sort(c); }

    void m(List<Integer> ints, List<String> strs) {
        String a = id("x");
        Integer b = id(5);
        List<Integer> c = Arrays.asList(3, 1, 2);
        Integer d = maxOf(ints);
        Integer e = Collections.max(ints);
        TimeUnit f = first(TimeUnit.class);
        Integer g = clamp(5, 1, 9);
        sortDesc(strs, (x, y) -> y.compareTo(x));
        Optional<Integer> h = Optional.of("x").map(s -> s.length());
        Map<Integer, List<Integer>> i = ints.stream().collect(Collectors.groupingBy(n -> n % 2));
        Map<Integer, List<String>> j = strs.stream().collect(Collectors.groupingBy(String::length));
        List<String> k = ints.stream().map(String::valueOf).collect(Collectors.toList());
        Stream.of("a", "b").map(s -> s.length()).collect(Collectors.toList()).get(0).intValue();
    }
}
"#;
    let fixture = Fixture::new(&[("T.java", source)]);
    if !fixture.has_stdlib() {
        eprintln!("skipping: no JDK standard library");
        return;
    }
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    assert!(
        found.is_empty(),
        "javac accepts this file, so these are false positives: {:?}",
        found.iter().map(diagnostics::describe).collect::<Vec<_>>()
    );
}

#[test]
fn diagnostics_say_nothing_about_unresolved_code() {
    // Nothing about `Missing` can be checked, so only the unknown type is
    // reported: no invented member or argument errors.
    let fixture = Fixture::new(&[(
        "T.java",
        "public class T {\n  void m() {\n    Missing x = null;\n    x.anything(1, 2, 3);\n    int y = x.field;\n  }\n}",
    )]);
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let codes: Vec<&str> = found.iter().filter_map(diagnostics::code_of).collect();
    assert_eq!(
        codes,
        vec![diagnostics::codes::UNKNOWN_TYPE],
        "{:?}",
        found.iter().map(diagnostics::describe).collect::<Vec<_>>()
    );
}

#[test]
fn diagnostics_flag_the_errors_a_student_makes() {
    let cases: &[(&str, &str)] = &[
        ("int x = \"text\";", diagnostics::codes::INCOMPATIBLE_TYPES),
        ("int x = notDeclared;", diagnostics::codes::UNKNOWN_SYMBOL),
        ("Undeclared y = null;", diagnostics::codes::UNKNOWN_TYPE),
        (
            "int a = 1; int a = 2;",
            diagnostics::codes::DUPLICATE_DECLARATION,
        ),
        ("this.missingMethod();", diagnostics::codes::UNKNOWN_MEMBER),
        ("helper(1, 2, 3);", diagnostics::codes::ARGUMENT_COUNT),
        ("helper(\"wrong\");", diagnostics::codes::ARGUMENT_TYPE),
    ];
    for (body, expected) in cases {
        let source = format!(
            "public class T {{\n  void helper(int a) {{}}\n  void m() {{\n    {body}\n  }}\n}}"
        );
        let fixture = Fixture::new(&[("T.java", source.as_str())]);
        let file = fixture.file("T.java");
        let found = diagnostics::diagnostics(&fixture.db, file);
        let codes: Vec<&str> = found.iter().filter_map(diagnostics::code_of).collect();
        assert!(
            codes.contains(expected),
            "expected {expected} for {body:?}, got {:?}",
            found.iter().map(diagnostics::describe).collect::<Vec<_>>()
        );
    }
}

#[test]
fn diagnostics_reject_java_8_violations() {
    use project_model::Config;
    let source = "public class T {\n  void m() {\n    Runnable r = () -> {};\n  }\n}";
    // At level 8 a lambda is fine.
    let fixture = Fixture::new(&[("T.java", source)]);
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let codes: Vec<&str> = found.iter().filter_map(diagnostics::code_of).collect();
    assert!(
        !codes.contains(&diagnostics::codes::LANGUAGE_LEVEL),
        "{codes:?}"
    );

    // At level 7 it is not.
    let fixture = Fixture::with_config(
        &[("T.java", source)],
        Config {
            source_level: Some(7),
            ..Config::default()
        },
    );
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let codes: Vec<&str> = found.iter().filter_map(diagnostics::code_of).collect();
    assert!(
        codes.contains(&diagnostics::codes::LANGUAGE_LEVEL),
        "{codes:?}"
    );
}

#[test]
fn code_actions_offer_the_missing_import() {
    let fixture = Fixture::new(&[(
        "T.java",
        "public class T {\n  void m() {\n    ArrayList<String> x = null;\n  }\n}",
    )]);
    if !fixture.has_stdlib() {
        return;
    }
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let unknown: Vec<_> = found
        .iter()
        .filter(|d| diagnostics::code_of(d) == Some(diagnostics::codes::UNKNOWN_TYPE))
        .cloned()
        .collect();
    assert!(!unknown.is_empty(), "the missing type is reported");
    let actions = code_actions::code_actions(&fixture.db, file, unknown[0].range, &unknown);
    let titles: Vec<String> = actions
        .iter()
        .map(|action| match action {
            lsp_types::CodeActionOrCommand::CodeAction(a) => a.title.clone(),
            lsp_types::CodeActionOrCommand::Command(c) => c.title.clone(),
        })
        .collect();
    assert!(
        titles.iter().any(|t| t == "Import `java.util.ArrayList`"),
        "{titles:?}"
    );
}

#[test]
fn code_actions_generate_constructors_and_accessors() {
    let fixture = Fixture::new(&[(
        "Point.java",
        "public class Point {\n  private int x;\n  private int y;\n}",
    )]);
    let file = fixture.file("Point.java");
    let range = Range {
        start: Position {
            line: 1,
            character: 4,
        },
        end: Position {
            line: 1,
            character: 4,
        },
    };
    let actions = code_actions::code_actions(&fixture.db, file, range, &[]);
    let titles: Vec<String> = actions
        .iter()
        .filter_map(|action| match action {
            lsp_types::CodeActionOrCommand::CodeAction(a) => Some(a.title.clone()),
            _ => None,
        })
        .collect();
    assert!(
        titles.iter().any(|t| t.starts_with("Generate constructor")),
        "{titles:?}"
    );
    assert!(
        titles.iter().any(|t| t == "Generate getters and setters"),
        "{titles:?}"
    );

    // The generated text has to be the Java a reader expects.
    let accessors = actions
        .iter()
        .find_map(|action| match action {
            lsp_types::CodeActionOrCommand::CodeAction(a)
                if a.title == "Generate getters and setters" =>
            {
                a.edit.clone()
            }
            _ => None,
        })
        .unwrap();
    let text = accessors
        .changes
        .unwrap()
        .into_values()
        .next()
        .unwrap()
        .remove(0)
        .new_text;
    assert!(text.contains("public int getX()"), "{text}");
    assert!(text.contains("public void setX(int x)"), "{text}");
    assert!(text.contains("this.x = x;"), "{text}");
}

#[test]
fn organize_imports_sorts_and_drops_the_unused() {
    let source = "package ds;\n\nimport java.util.Map;\nimport java.util.ArrayList;\nimport java.util.List;\n\npublic class T {\n  List<String> items = new ArrayList<String>();\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let file = fixture.file("T.java");
    let edit = imports::organize_imports(&fixture.db, file).expect("imports need organizing");
    assert_eq!(
        edit.new_text,
        "import java.util.ArrayList;\nimport java.util.List;\n"
    );
}

#[test]
fn semantic_tokens_classify_by_resolution() {
    let source = "package ds;\npublic class T {\n  private int field;\n  void m(String param) {\n    int local = field;\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    let file = fixture.file("T.java");
    let found = tokens::semantic_tokens(&fixture.db, file).expect("tokens");
    assert!(!found.data.is_empty());
    // Deltas must be non-negative and ordered, or the client misrenders.
    let mut line = 0u32;
    let mut start = 0u32;
    for token in &found.data {
        line += token.delta_line;
        if token.delta_line == 0 {
            start += token.delta_start;
        } else {
            start = token.delta_start;
        }
        assert!(token.length > 0);
        assert!((token.token_type as usize) < tokens::TOKEN_TYPES.len());
    }
    let _ = (line, start);
}

#[test]
fn implementations_find_subclasses_and_overrides() {
    let files = [
        (
            "Shape.java",
            "public interface Shape {\n  double area();\n}",
        ),
        (
            "Circle.java",
            "public class Circle implements Shape {\n  public double area() { return 1; }\n}",
        ),
        (
            "Square.java",
            "public class Square implements Shape {\n  public double area() { return 2; }\n}",
        ),
    ];
    let fixture = Fixture::new(&files);
    let file = fixture.file("Shape.java");
    let offset = fixture.at("Shape.java", "Shape");
    let found = navigation::implementations(&fixture.db, file, offset);
    let names: Vec<&str> = found
        .iter()
        .map(|l| l.uri.as_str().rsplit('/').next().unwrap())
        .collect();
    assert!(names.contains(&"Circle.java"), "{names:?}");
    assert!(names.contains(&"Square.java"), "{names:?}");

    let offset = fixture.at("Shape.java", "area");
    let found = navigation::implementations(&fixture.db, file, offset);
    assert_eq!(found.len(), 2, "both overrides: {found:?}");
}

#[test]
fn document_highlights_cover_one_file_only() {
    let files = [
        (
            "A.java",
            "public class A {\n  int value;\n  int read() { return value; }\n}",
        ),
        (
            "B.java",
            "public class B {\n  void m(A a) { int x = a.value; }\n}",
        ),
    ];
    let fixture = Fixture::new(&files);
    let file = fixture.file("A.java");
    let offset = fixture.at("A.java", "value");
    let found = navigation::document_highlights(&fixture.db, file, offset);
    assert_eq!(found.len(), 2, "declaration and the read in A: {found:?}");
}

/// A method reference or lambda takes its type from the call it is an argument
/// to, while that call's type depends on its arguments. That mutual dependency
/// once recursed until the stack was exhausted, on real code taken from the
/// Eclipse JDT language server.
#[test]
fn inference_terminates_on_self_referential_argument_types() {
    let sources = [
        "import java.util.*;\npublic class T {\n  void m(Set<String> names) {\n    String[] array = names.toArray(String[]::new);\n  }\n}",
        "import java.util.*;\npublic class T {\n  void m(List<String> names) {\n    names.sort((a, b) -> a.compareTo(b));\n  }\n}",
        "import java.util.*;\npublic class T {\n  void m(List<String> names) {\n    names.forEach(name -> names.forEach(other -> System.out.println(other)));\n  }\n}",
        // Deeply nested calls, each depending on the next.
        "public class T {\n  int f(int a) { return a; }\n  void m() {\n    int x = f(f(f(f(f(f(f(f(f(f(1)))))))))); \n  }\n}",
    ];
    for source in sources {
        let fixture = Fixture::new(&[("T.java", source)]);
        let file = fixture.file("T.java");
        // Must return rather than recurse without bound.
        let _ = diagnostics::diagnostics(&fixture.db, file);
        let _ = symbols::document_symbols(&fixture.db, file);
        let _ = tokens::semantic_tokens(&fixture.db, file);
    }
}

/// Deeply nested syntax must not exhaust the stack in any of the walkers.
#[test]
fn deeply_nested_code_is_analysed_without_exhausting_the_stack() {
    let depth = 300;
    let body = format!(
        "{}int x = 1;{}",
        "if (true) { ".repeat(depth),
        "}".repeat(depth)
    );
    let source = format!("public class T {{\n  void m() {{\n{body}\n  }}\n}}");
    let fixture = Fixture::new(&[("T.java", source.as_str())]);
    let file = fixture.file("T.java");
    let _ = diagnostics::diagnostics(&fixture.db, file);

    // A long chain of member accesses, which recursion over expressions sees.
    let chain = format!(
        "public class T {{\n  T next;\n  void m() {{\n    T a = this{};\n  }}\n}}",
        ".next".repeat(200)
    );
    let fixture = Fixture::new(&[("T.java", chain.as_str())]);
    let file = fixture.file("T.java");
    let _ = diagnostics::diagnostics(&fixture.db, file);
    let _ = tokens::semantic_tokens(&fixture.db, file);

    // Deeply nested generics.
    let nested = format!(
        "import java.util.*;\npublic class T {{\n  {}List<String>{} x;\n}}",
        "List<".repeat(60),
        ">".repeat(60)
    );
    let fixture = Fixture::new(&[("T.java", nested.as_str())]);
    let file = fixture.file("T.java");
    let _ = diagnostics::diagnostics(&fixture.db, file);
}

/// Class files carry neither parameter names nor javadoc. The JDK's `src.zip`
/// does, and attaching it is what turns `add(String arg0)` with no
/// documentation into `add(String e)` with the library's own prose.
#[test]
fn standard_library_members_carry_names_and_documentation() {
    let fixture = Fixture::new(&[(
        "T.java",
        "import java.util.*;\npublic class T {\n  void m() {\n    List<String> names = new ArrayList<String>();\n    names.add(\"x\");\n  }\n}",
    )]);
    if !fixture.has_stdlib() {
        eprintln!("skipping: no JDK standard library");
        return;
    }
    let Some(list) = fixture.db.class("java.util.List") else {
        panic!("java.util.List should be on the classpath")
    };
    let add = list
        .methods_named("add")
        .into_iter()
        .find(|m| m.params.len() == 1)
        .expect("List.add(E)");
    if add.doc.is_none() && add.params[0].name.starts_with("arg") {
        // A JDK without src.zip is a valid installation; the server then falls
        // back to positional names, which is why this is a skip and not a
        // failure.
        eprintln!("skipping: this JDK has no src.zip attached");
        return;
    }
    assert_ne!(
        &*add.params[0].name, "arg0",
        "the parameter name should come from the JDK sources"
    );
    let doc = add.doc.as_deref().expect("javadoc for List.add");
    assert!(
        doc.contains("Appends") || doc.contains("end of this list"),
        "unexpected javadoc: {doc}"
    );

    // And it reaches hover, which is where a student sees it.
    let file = fixture.file("T.java");
    let offset = fixture.after("T.java", "names.ad");
    let hover = hover::hover(&fixture.db, file, offset).expect("hover on add");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        _ => panic!(),
    };
    // The call's own signature, with the receiver's type argument applied,
    // and the declaration it came from.
    assert!(text.contains("add(String e)"), "{text}");
    assert!(text.contains("declared as"), "{text}");
    assert!(text.contains("add(E e)"), "{text}");
    assert!(!text.contains("arg0"), "{text}");
    assert!(text.contains("list"), "javadoc should be rendered: {text}");

    // Signature help shows the real parameter name too.
    let fixture = Fixture::new(&[(
        "T.java",
        "import java.util.*;\npublic class T {\n  void m() {\n    List<String> names = new ArrayList<String>();\n    names.add($0);\n  }\n}",
    )]);
    let (file, offset) = fixture.cursor();
    let help = signature::signature_help(&fixture.db, file, offset).expect("signature help");
    assert!(
        help.signatures
            .iter()
            .any(|signature| !signature.label.contains("arg0")),
        "{:?}",
        help.signatures.iter().map(|s| &s.label).collect::<Vec<_>>()
    );
}

/// Lambda parameters are in scope inside the lambda body. Reusing a context
/// built at the enclosing statement once made every name in a lambda body
/// look undefined, which `javac` disagreed with on real code.
#[test]
fn lambda_parameters_are_in_scope_in_the_body() {
    let source = "import java.util.*;\npublic class T {\n  void m(List<String> names) {\n    names.forEach(name -> System.out.println(name));\n    Comparator<String> byLength = (a, b) -> a.length() - b.length();\n    Runnable r = () -> System.out.println(\"x\");\n  }\n}";
    let fixture = Fixture::new(&[("T.java", source)]);
    if !fixture.has_stdlib() {
        return;
    }
    let file = fixture.file("T.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    let messages: Vec<String> = found.iter().map(diagnostics::describe).collect();
    assert!(messages.is_empty(), "{messages:?}");

    // A typed lambda parameter resolves to its declared type, so its members
    // are offered.
    let labels = labels(&[(
        "T.java",
        "import java.util.*;\npublic class T {\n  void m() {\n    Comparator<String> c = (String a, String b) -> a.$0;\n  }\n}",
    )]);
    assert!(labels.contains(&"length".to_string()), "{labels:?}");
}

/// A lambda written without parameter types takes them from the functional
/// interface it targets, which is how `(a, b) -> a.compareTo(b)` is written.
#[test]
fn untyped_lambda_parameters_take_their_type_from_the_target_interface() {
    let fixture = Fixture::new(&[("T.java", "class T {}")]);
    let has_stdlib = fixture.has_stdlib();
    drop(fixture);
    if !has_stdlib {
        return;
    }
    // From a declared variable's type.
    let found = labels(&[(
        "T.java",
        "import java.util.*;\npublic class T {\n  void m() {\n    Comparator<String> c = (a, b) -> a.$0;\n  }\n}",
    )]);
    assert!(found.contains(&"compareTo".to_string()), "{found:?}");
    assert!(found.contains(&"length".to_string()), "{found:?}");

    // From the parameter type of the call the lambda is an argument to.
    let found = labels(&[(
        "T.java",
        "import java.util.*;\npublic class T {\n  void m(List<String> names) {\n    names.forEach(name -> name.$0);\n  }\n}",
    )]);
    assert!(found.contains(&"length".to_string()), "{found:?}");

    // A workspace functional interface works the same way.
    let found = labels(&[
        (
            "Visitor.java",
            "public interface Visitor<E> {\n  void visit(E item);\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Visitor<String> v = item -> item.$0;\n  }\n}",
        ),
    ]);
    assert!(found.contains(&"length".to_string()), "{found:?}");

    // An interface with two abstract methods is not functional, so nothing is
    // claimed about the parameter.
    let found = labels(&[
        (
            "Two.java",
            "public interface Two {\n  void a(String x);\n  void b(String y);\n}",
        ),
        (
            "T.java",
            "public class T {\n  void m() {\n    Two t = x -> x.$0;\n  }\n}",
        ),
    ]);
    assert!(found.is_empty(), "{found:?}");
}

/// A member that exists but is not visible is a different mistake from one
/// that does not exist, and gets a different message.
#[test]
fn inaccessible_members_are_named_as_such() {
    let files = [
        ("Other.java", "public class Other {\n  private int secret;\n  int visible;\n}"),
        ("User.java", "public class User {\n  void m(Other other) {\n    int a = other.secret;\n    int b = other.visible;\n  }\n}"),
    ];
    let fixture = Fixture::new(&files);
    let file = fixture.file("User.java");
    let found = diagnostics::diagnostics(&fixture.db, file);
    assert_eq!(
        found.len(),
        1,
        "{:?}",
        found.iter().map(diagnostics::describe).collect::<Vec<_>>()
    );
    assert_eq!(
        diagnostics::code_of(&found[0]),
        Some(diagnostics::codes::INACCESSIBLE)
    );
    assert!(found[0].message.contains("secret"), "{}", found[0].message);
    assert!(
        found[0].message.contains("not accessible"),
        "{}",
        found[0].message
    );

    // And it is not offered by completion in the first place.
    let fixture = Fixture::new(&[
        (
            "Other.java",
            "public class Other {\n  private int secret;\n  int visible;\n}",
        ),
        (
            "User.java",
            "public class User {\n  void m(Other other) {\n    other.$0\n  }\n}",
        ),
    ]);
    let (file, offset) = fixture.cursor();
    let labels: Vec<String> = completion::completions(&fixture.db, file, offset, settings())
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"visible".to_string()), "{labels:?}");
    assert!(!labels.contains(&"secret".to_string()), "{labels:?}");
}

#[test]
fn nested_classes_can_access_private_members_of_their_nest() {
    let fixture = Fixture::new(&[(
        "Outer.java",
        r#"
        class Outer {
            private int outer;
            void print(Inner n) { int x = n.value; int y = Inner.count; }
            static class Inner {
                private int value;
                private static int count;
                void read(Outer o, Sibling s) { int x = o.outer; int y = s.value; }
            }
            static class Sibling { private int value; }
        }
    "#,
    )]);
    let found = diagnostics::diagnostics(&fixture.db, fixture.file("Outer.java"));
    assert!(
        found.is_empty(),
        "{:?}",
        found.iter().map(diagnostics::describe).collect::<Vec<_>>()
    );
}

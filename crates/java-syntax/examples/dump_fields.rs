//! Prints each node with its field name, to confirm grammar field names.
use java_syntax::{ast, SourceFile};

fn main() {
    let src = std::env::args().nth(1).unwrap();
    let file = SourceFile::new(src, 0);
    walk(file.tree().root_node(), file.text(), 0, None);
    for e in file.syntax_errors() {
        println!("ERR {:?} {}", (e.start, e.end), e.message);
    }
}

fn walk(node: java_syntax::Node<'_>, src: &str, depth: usize, field: Option<&str>) {
    println!(
        "{:indent$}{}{} {:?}",
        "",
        field.map(|f| format!("{f}: ")).unwrap_or_default(),
        node.kind(),
        if node.child_count() == 0 {
            ast::text(node, src)
        } else {
            ""
        },
        indent = depth * 2
    );
    // children are indexed so field names can be queried per child
    for i in 0..node.child_count() {
        let child = node.child(i).unwrap();
        walk(child, src, depth + 1, node.field_name_for_child(i as u32));
    }
}

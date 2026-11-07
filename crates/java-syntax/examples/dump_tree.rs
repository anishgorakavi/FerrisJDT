//! Prints the parse tree of a Java file: `cargo run --example dump_tree -- File.java`

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dump_tree <file.java>");
    let text = std::fs::read_to_string(&path).expect("file should be readable");
    let file = java_syntax::SourceFile::new(text, 0);
    print!(
        "{}",
        java_syntax::ast::debug_tree(file.tree().root_node(), file.text())
    );
    for error in file.syntax_errors() {
        let position = file.position_at(error.start);
        println!(
            "error {}:{} {}",
            position.line + 1,
            position.character + 1,
            error.message
        );
    }
}

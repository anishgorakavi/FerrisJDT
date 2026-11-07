//! End-to-end tests over the real JSON-RPC transport.
//!
//! The server binary is spawned and driven exactly as an editor would drive
//! it, which is the only way to check the parts no unit test reaches: the
//! framing, the capability handshake, incremental `didChange` application, and
//! the rule that nothing but protocol may appear on stdout.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    process: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Server {
    fn start(root: &Path) -> Server {
        let mut process = Command::new(env!("CARGO_BIN_EXE_ferrisjdt"))
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("FERRISJDT_LOG", "error")
            .spawn()
            .expect("the server binary should start");
        let stdin = process.stdin.take().unwrap();
        let stdout = BufReader::new(process.stdout.take().unwrap());
        let mut server = Server {
            process,
            stdin,
            stdout,
            next_id: 1,
        };
        let uri = uri_for(root);
        let result = server.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri,
                "capabilities": {
                    "textDocument": {
                        "completion": {
                            "completionItem": { "snippetSupport": true }
                        },
                        "definition": { "linkSupport": true },
                        "rename": { "prepareSupport": true }
                    }
                },
                "workspaceFolders": [{ "uri": uri, "name": "test" }]
            }),
        );
        assert!(
            result["capabilities"]["completionProvider"].is_object(),
            "completion must be advertised: {result}"
        );
        assert_eq!(result["serverInfo"]["name"], "ferrisjdt");
        server.notify("initialized", json!({}));
        server
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_string(message).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        // Notifications may arrive before the response; skip past them.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {method}");
            let message = self.read_message();
            if message["id"] == json!(id) {
                if let Some(error) = message.get("error") {
                    if !error.is_null() {
                        panic!("{method} failed: {error}");
                    }
                }
                return message["result"].clone();
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Waits for a notification with the given method.
    fn await_notification(&mut self, method: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {method}");
            let message = self.read_message();
            if message["method"] == json!(method) {
                return message["params"].clone();
            }
        }
    }

    fn read_message(&mut self) -> Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("server stdout");
            assert!(read > 0, "the server closed its output");
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                length = Some(value.parse::<usize>().expect("a numeric content length"));
            }
        }
        let length = length.expect("every message is framed with a content length");
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).expect("a JSON body")
    }

    fn shutdown(mut self) -> String {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let status = self.process.wait().expect("the server should exit");
        assert!(status.success(), "exited with {status:?}");
        let mut stderr = String::new();
        if let Some(mut handle) = self.process.stderr.take() {
            let _ = handle.read_to_string(&mut stderr);
        }
        stderr
    }
}

fn uri_for(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join("ferrisjdt-protocol")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

const TREE_SOURCE: &str = "package ds;

public class Tree {
    private Node root;
    private int size;

    /** Adds a value. */
    public boolean add(int value) {
        size++;
        return true;
    }

    public int size() {
        return size;
    }
}
";

const NODE_SOURCE: &str = "package ds;

public class Node {
    int value;
    Node left;
    Node right;
}
";

#[test]
fn completes_hovers_navigates_and_diagnoses_over_the_protocol() {
    let root = scratch("full");
    let tree_path = root.join("Tree.java");
    let node_path = root.join("Node.java");
    std::fs::write(&tree_path, TREE_SOURCE).unwrap();
    std::fs::write(&node_path, NODE_SOURCE).unwrap();
    let mut server = Server::start(&root);
    let tree_uri = uri_for(&tree_path);

    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": tree_uri,
                "languageId": "java",
                "version": 1,
                "text": TREE_SOURCE
            }
        }),
    );

    // Diagnostics arrive unprompted, and this file is correct.
    let published = server.await_notification("textDocument/publishDiagnostics");
    assert_eq!(published["uri"], json!(tree_uri));
    let diagnostics = published["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics.is_empty(),
        "expected a clean file: {diagnostics:?}"
    );

    // Document symbols describe the class and its members.
    let symbols = server.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": tree_uri } }),
    );
    let names: Vec<String> = symbols[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| child["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(symbols[0]["name"], json!("Tree"));
    assert!(names.contains(&"root".to_string()), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("add(")), "{names:?}");

    // Hover on `size` in `return size;` (line 13, zero-based).
    let hover = server.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": tree_uri },
            "position": { "line": 13, "character": 16 }
        }),
    );
    let text = hover["contents"]["value"].as_str().unwrap_or("");
    assert!(text.contains("int size"), "hover said: {text}");

    // Definition of `Node` in the field declaration (line 3).
    let definition = server.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": tree_uri },
            "position": { "line": 3, "character": 13 }
        }),
    );
    let target = definition[0]["targetUri"].as_str().unwrap();
    assert!(target.ends_with("Node.java"), "went to {target}");

    // References to the field `size`: its declaration, `size++`, and
    // `return size`. The method also called `size` is a different symbol and
    // must not be included.
    let references = server.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": tree_uri },
            "position": { "line": 4, "character": 16 },
            "context": { "includeDeclaration": true }
        }),
    );
    let lines: Vec<i64> = references
        .as_array()
        .unwrap()
        .iter()
        .map(|location| location["range"]["start"]["line"].as_i64().unwrap())
        .collect();
    assert_eq!(lines, vec![4, 8, 13], "{references}");

    // An incremental change that introduces an error, then its diagnostic.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": tree_uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 8, "character": 8 },
                    "end": { "line": 8, "character": 15 }
                },
                "text": "notAField = 1;"
            }]
        }),
    );
    let published = server.await_notification("textDocument/publishDiagnostics");
    let diagnostics = published["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d["message"].as_str().unwrap_or("").contains("notAField")),
        "{diagnostics:?}"
    );
    assert_eq!(published["version"], json!(2));

    // Undo it, and the diagnostic goes away.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": tree_uri, "version": 3 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 8, "character": 8 },
                    "end": { "line": 8, "character": 22 }
                },
                "text": "size++;"
            }]
        }),
    );
    let published = server.await_notification("textDocument/publishDiagnostics");
    assert!(
        published["diagnostics"].as_array().unwrap().is_empty(),
        "{published}"
    );

    let stderr = server.shutdown();
    assert!(
        !stderr.contains("panicked"),
        "the server panicked:\n{stderr}"
    );
}

#[test]
fn completes_member_access_over_the_protocol() {
    let root = scratch("completion");
    let node_path = root.join("Node.java");
    std::fs::write(&node_path, NODE_SOURCE).unwrap();
    let user_path = root.join("User.java");
    let original = "package ds;\n\npublic class User {\n    void m(Node n) {\n        \n    }\n}\n";
    std::fs::write(&user_path, original).unwrap();

    let mut server = Server::start(&root);
    let uri = uri_for(&user_path);
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "java", "version": 1, "text": original }
        }),
    );
    // Type `n.` on the empty line, as an editor would.
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 4, "character": 8 },
                    "end": { "line": 4, "character": 8 }
                },
                "text": "n."
            }]
        }),
    );
    let completion = server.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 4, "character": 10 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }),
    );
    let items = completion["items"].as_array().expect("a completion list");
    let labels: Vec<&str> = items
        .iter()
        .map(|item| item["label"].as_str().unwrap())
        .collect();
    for expected in ["value", "left", "right"] {
        assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
    }
    // Object's methods are inherited and must be there too, ranked lower.
    assert!(labels.contains(&"equals"), "{labels:?}");

    // Each item must carry an edit that replaces the right range.
    let value = items.iter().find(|i| i["label"] == json!("value")).unwrap();
    assert_eq!(value["textEdit"]["range"]["start"]["character"], json!(10));

    let stderr = server.shutdown();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn renames_across_files_over_the_protocol() {
    let root = scratch("rename");
    std::fs::write(root.join("Node.java"), NODE_SOURCE).unwrap();
    let user = "package ds;\npublic class User {\n    int read(Node n) { return n.value; }\n}\n";
    std::fs::write(root.join("User.java"), user).unwrap();

    let mut server = Server::start(&root);
    let node_uri = uri_for(&root.join("Node.java"));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": node_uri, "languageId": "java", "version": 1, "text": NODE_SOURCE }
        }),
    );

    let prepared = server.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": { "uri": node_uri },
            "position": { "line": 3, "character": 8 }
        }),
    );
    assert_eq!(prepared["placeholder"], json!("value"));

    let edit = server.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": node_uri },
            "position": { "line": 3, "character": 8 },
            "newName": "payload"
        }),
    );
    let changes = edit["changes"].as_object().expect("changes");
    assert_eq!(changes.len(), 2, "both files change: {changes:?}");
    let total: usize = changes
        .values()
        .map(|edits| edits.as_array().unwrap().len())
        .sum();
    assert_eq!(total, 2);

    // A rename to a keyword is refused with an error, not applied silently.
    let id = server.next_id;
    server.next_id += 1;
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": node_uri },
            "position": { "line": 3, "character": 8 },
            "newName": "class"
        }
    }));
    let response = loop {
        let message = server.read_message();
        if message["id"] == json!(id) {
            break message;
        }
    };
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("not a valid Java identifier"));

    let stderr = server.shutdown();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn stdout_carries_only_protocol_even_with_logging_on() {
    // Anything written to stdout other than framed messages corrupts the
    // stream, so this checks it with the loudest log level.
    let root = scratch("logging");
    std::fs::write(root.join("Node.java"), NODE_SOURCE).unwrap();
    let mut process = Command::new(env!("CARGO_BIN_EXE_ferrisjdt"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("FERRISJDT_LOG", "trace")
        .spawn()
        .unwrap();
    let mut stdin = process.stdin.take().unwrap();
    let uri = uri_for(&root);
    for message in [
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": uri, "capabilities": {} }
        }),
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null }),
        json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    ] {
        let body = serde_json::to_string(&message).unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    }
    stdin.flush().unwrap();
    drop(stdin);
    let output = process.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Every byte of stdout belongs to a framed message.
    let mut rest = stdout.as_ref();
    let mut messages = 0;
    while !rest.is_empty() {
        let header_end = rest
            .find("\r\n\r\n")
            .unwrap_or_else(|| panic!("stdout is not valid framing; remainder was {rest:?}"));
        let header = &rest[..header_end];
        let length: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap_or_else(|| panic!("unexpected header {header:?}"))
            .trim()
            .parse()
            .unwrap();
        rest = &rest[header_end + 4 + length..];
        messages += 1;
    }
    assert_eq!(messages, 2, "initialize and shutdown responses");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ready"),
        "logs should go to stderr"
    );
}

#[test]
fn survives_a_torrent_of_edits_without_falling_behind() {
    // Simulates fast typing: each keystroke is a didChange, and after the
    // burst the server must still answer correctly.
    let root = scratch("typing");
    std::fs::write(root.join("Node.java"), NODE_SOURCE).unwrap();
    let path = root.join("Typing.java");
    let original = "package ds;\npublic class Typing {\n    void m(Node n) {\n    }\n}\n";
    std::fs::write(&path, original).unwrap();

    let mut server = Server::start(&root);
    let uri = uri_for(&path);
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "java", "version": 1, "text": original }
        }),
    );

    let typed = "int v = n.value;";
    let mut version = 1;
    for (index, ch) in typed.char_indices() {
        version += 1;
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 2, "character": 20 + index as u32 },
                        "end": { "line": 2, "character": 20 + index as u32 }
                    },
                    "text": ch.to_string()
                }]
            }),
        );
    }
    // One request after the burst: the document must be exactly what was typed
    // and still analysable.
    let started = Instant::now();
    let hover = server.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 2, "character": 31 }
        }),
    );
    let elapsed = started.elapsed();
    let text = hover["contents"]["value"].as_str().unwrap_or("");
    assert!(
        text.contains("int value"),
        "hover after typing said: {text}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "hover took {elapsed:?} after a burst of edits"
    );

    let stderr = server.shutdown();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn reports_status_and_handles_unknown_requests() {
    let root = scratch("status");
    std::fs::write(root.join("Node.java"), NODE_SOURCE).unwrap();
    let mut server = Server::start(&root);
    let status = server.request("ferrisjdt/status", json!({}));
    assert_eq!(status["sourceLevel"], json!("8"));
    assert!(status["classpathClasses"].as_u64().unwrap_or(0) > 0 || status["jdkHome"].is_null());

    // An unsupported method gets an error, not a crash.
    let id = server.next_id;
    server.next_id += 1;
    server.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/inlayHint", "params": {}
    }));
    let response = loop {
        let message = server.read_message();
        if message["id"] == json!(id) {
            break message;
        }
    };
    assert_eq!(response["error"]["code"], json!(-32601));

    let stderr = server.shutdown();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

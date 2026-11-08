# AGENTS.md — FerrisJDT

FerrisJDT is a lightweight Java language server in Rust: tree-sitter parsing, the JDK's class
files read directly, and its own semantic layer (scopes, types, generic substitution, overload
resolution, inference). No JVM. It is built for Java 8 coursework (generic collections,
`Comparable`, trees and nodes, half-typed code), and talks plain LSP over stdio.

Read `ARCHITECTURE.md` for the design, `TREE.md` for what every file does, `FEATURES.md` for what
is supported and which test covers it, and `LIMITATIONS.md` for what is not.

## Layout

This repo is one project in a workspace. Siblings (not part of this git repo):

* `../vscode_extension` — the VS Code client (LSP plus Run/Debug). Bundles a copy of this binary.
* `../eclipse.jdt.ls` — a clone of JDTLS, used only as a benchmark and corpus. Never edit it.

## Commands

```sh
cargo build --release                      # the binary editors run: target/release/ferrisjdt
cargo test --release                       # everything; the corpus test alone takes ~50 s
cargo test --release -p java-semantic --test generics     # one test file
cargo test --release -p java-ide --test features <name>   # one test by name
cargo fmt --all && cargo clippy --release --all-targets   # CI runs both with RUSTFLAGS=-D warnings
make check                                 # fmt + clippy -D warnings + release tests
make selfcheck                             # index tests/fixtures/library and report timings
ferrisjdt --self-check <dir> --verbose     # where was the JDK found, what resolved
```

Always test with `--release`: the corpus tests parse a thousand third-party files.
Tests that need the JDK library skip (do not fail) when none is found; CI points
`FERRISJDT_TEST_RT_JAR` at a JDK 8 `rt.jar`. A skipped test proves nothing, so check the JDK was found.

**A rebuilt test run does not update the editor's binary.** Editors run
`target/release/ferrisjdt` (Neovim: `~/.config/nvim/lua/plugins/ferrisjdt.lua` searches
`vim.g.ferrisjdt_path`, `PATH`, `~/.local/bin`, then this build; the VS Code extension uses its own copy
in `../vscode_extension/bin`, refreshed with `npm run bundle-server` there). Rebuild with
`cargo build --release` before checking a fix over LSP, and say which binary you exercised.

## Crates and the layering rule

Dependencies run one way: `java-syntax`, `java-classfile` → `project-model` → `java-semantic` →
`java-ide` → `ferrisjdt` (the binary). A lower crate must not know about a higher one:
`java-semantic` knows nothing of LSP, `java-ide` knows nothing of transports. Keep it that way.

## Invariants — do not break these

1. **Unknown stays unknown.** `Type::Unknown` means resolution did not succeed. Propagate it; never
   replace it with a guess. Every diagnostic and completion check must *stand down* on a type that
   `has_unknown()`. A false error on valid code is worse than a missed error. Incomplete (half-typed)
   code must produce no wrong diagnostics, only fewer completions.
2. **stdout is the protocol and nothing else.** Logs go to stderr (`FERRISJDT_LOG=debug`).
   `tests/protocol.rs` enforces this.
3. **Untrusted input never panics.** Class files, JARs, and source are bounds-checked and return
   errors. Recursion is bounded by `MAX_EXPRESSION_DEPTH` / `MAX_HIERARCHY_DEPTH` plus the
   `active_calls` cycle guard in `infer.rs`.
4. **No JVM, ever.** Nothing here may spawn `java`.
5. **Features are advertised only when a handler is useful** (`capabilities.rs`).

## The type model — gotchas that have caused real bugs

* **`Substitution` maps variable *names* to types, not declarations.** A static method's own `<T>`
  and its class's `T` collide. `members.rs` therefore substitutes nothing into a static method and
  removes a method's own type-parameter names from the receiver substitution. Any new substitution
  site must respect this. `TypeVar` equality also ignores bounds on purpose.
* **`Type::upper_bound()` on a declared type variable destroys it** (a free `T` becomes `Object`).
  Use it for member lookup on an *actual* type, never when matching a *declared* parameter.
* **A method's type parameters must be in scope before its parameter and local types are lowered**
  (`scope.rs::method_type_parameters`); otherwise `T item` silently becomes unknown.
* **Generic method inference** lives in `solve.rs` (the solver) and `infer.rs::solve_call` (the
  order): typed args (varargs expanded) → expected type → nested calls inferred against the parameter
  they fill → lambda and method-reference results. An unbounded variable nothing fixes is `Unknown`,
  not `Object`; a bounded one takes its bound. Overload applicability goes through
  `overload::instantiated_params`, so `T` accepts a `String`.
* Lambda parameters are typed in two places: `scope.rs::resolve_lambda_parameter_types` (cursor inside
  the lambda) and `infer.rs::with_locals` (evaluating a body to learn what it returns). Keep both
  working when you change either.

Symptom → where to look: wrong member list after a call → `infer.rs` / `solve.rs`; false
"argument N expects T" → `overload.rs`; `x.` offers nothing inside a generic method → `scope.rs`;
wrong overload → `overload.rs::select`; diagnostics too eager → `java-ide/src/diagnostics.rs`.

## Tests

* Add a test for every behaviour change, next to its peers: `crates/java-semantic/tests/inference.rs`
  and `generics.rs` (types), `crates/java-ide/tests/features.rs` (what an editor shows),
  `crates/ferrisjdt/tests/protocol.rs` (the real binary over stdio).
* `Fixture::new(&[("T.java", src)])` builds a database from in-memory files; `$0` marks the cursor.
  `ast::expression_ending_at` returns the *widest* expression ending at the cursor, so
  `(a, b) -> b$0` yields the whole lambda — put the cursor mid-expression (`b$0.length()`).
* A "no false diagnostics" test must use code `javac` accepts. Verify with `javac` first.
* Compare before/after on realistic code, not only unit tests: drive the built binary over LSP
  (initialize → didOpen → completion/hover → diagnostics) on a `javac`-valid file.
* To inspect a syntax tree: `cargo run -q -p java-syntax --example dump_fields -- '<java source text>'`
  (the argument is the source text, not a path).

## Keep the docs true

`TREE.md` must change in the same commit that adds, moves, or repurposes a file. If behaviour
changes, update `FEATURES.md` and `LIMITATIONS.md` too — a limitation that no longer holds, or a
"no false diagnostics" claim that does not, is a bug in the docs.

## Style

* Four spaces, 100 columns (`rustfmt.toml`); the only tabs are Makefile recipe lines.
* Every file starts with a `//!` purpose line; every function has a `///` comment that says what it is
  for. Inline comments explain *why*, never what. No TODOs, commented-out code, or chatter.
* Smallest change that works; match the surrounding code. No new dependency without a reason.
* Clippy must be clean under `-D warnings`.

## Git

Branch `main`. Run `git status` before committing and stage only your own files — other changes may be
in progress in the tree. Commit or push only when asked. End commit messages with the
`Co-Authored-By:` line the session's instructions specify.

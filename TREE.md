# TREE

FerrisJDT: a lightweight Java language server in Rust. Parses with tree-sitter,
reads the JDK's class files directly, and builds the semantic layer — scopes,
types, substitution, overload resolution — itself. No JVM.

```text
Cargo.toml                Workspace: members, shared dependency versions, release profile.
Makefile                  build, test, lint, install, self-check, and benchmark targets.
rustfmt.toml              Formatting: four spaces, 100 columns, no tabs.
README.md                 Install, Neovim setup, project configuration, usage.
ARCHITECTURE.md           Components, the type model, generic substitution, incremental analysis.
TREE.md                   This map.
FEATURES.md               Implemented, partial, and unsupported, with the tests covering each.
BENCHMARKS.md             Measurements, methodology, and how to reproduce them.
LIMITATIONS.md            Semantic gaps, project model gaps, performance characteristics.
.github/workflows/ci.yml  CI: fmt, clippy, tests on Linux and macOS, javac cross-check.

crates/java-syntax/                 Parsing and position mapping. Knows nothing of types.
  src/lib.rs                        Crate root; exposes the tree-sitter Java grammar.
  src/source.rs                     An open document: text, parse tree, and line index kept in
                                    step. Applies incremental edits; collects parser errors.
  src/line_index.rs                 Byte offset to LSP position and back, counting UTF-16 code
                                    units. ASCII-only lines are flagged to skip scanning.
  src/ast.rs                        Helpers over tree-sitter nodes: node at an offset, ancestor
                                    walks, field access, the widest expression ending at a point.
  examples/dump_tree.rs             Prints a file's parse tree; used to develop against the grammar.
  examples/dump_fields.rs           Prints the tree with grammar field names, to verify field names.
  tests/parsing.rs                  Incremental edits match a fresh parse, error recovery,
                                    adversarial input.

crates/java-classfile/              Reads .class files and JARs. Knows nothing of classpaths.
  src/lib.rs                        Crate root; internal, binary, and source name conversion.
  src/reader.rs                     Bounds-checked byte cursor and JVM modified-UTF-8 decoding;
                                    every read errors rather than panicking.
  src/constant_pool.rs              Constant pool parsing, retaining only the kinds used while
                                    keeping indices aligned.
  src/signature.rs                  Type descriptor and generic signature parsers, producing a
                                    syntactic form with no notion of resolution.
  src/class.rs                      Class file decoding: flags, fields, methods, attributes.
                                    Method bodies are skipped entirely.
  src/jar.rs                        A minimal read-only ZIP reader; names share one arena so
                                    indexing rt.jar costs about a megabyte.
  src/error.rs                      The crate's error type; untrusted input never panics.
  tests/signature.rs                Descriptor and signature parsing, wildcards, bounds, nested
                                    types, malformed input.
  tests/jar.rs                      Decodes the real JDK 8 rt.jar, plus truncated and corrupted
                                    input.

crates/project-model/               What the project is: roots, JDK, classpath, language level.
  src/lib.rs                        Crate root.
  src/config.rs                     Client and file settings, and the source level that decides
                                    which language constructs are allowed.
  src/jdk.rs                        Finds a JDK and its library: rt.jar on 8, else ct.sym.
  src/project.rs                    Source roots, classpath JARs, and the bounded,
                                    symlink-refusing source file walk.
  tests/discovery.rs                Layout detection, exclusions, symlink refusal, limits.

crates/java-semantic/               The semantic core. Knows nothing of LSP.
  src/lib.rs                        Crate root and the recursion bounds every walker shares.
  src/name.rs                       Reference-counted names; binary, simple, and package forms.
  src/types.rs                      The type model: primitives, classes, arrays, type variables,
                                    wildcards, plus erasure, substitution, boxing, promotion.
  src/symbols.rs                    Declarations as the resolver sees them, and where each came
                                    from.
  src/classpath.rs                  Indexes archives by class name and decodes classes on demand,
                                    converting signatures into the type model.
  src/attach.rs                     Recovers parameter names and javadoc from a JDK's src.zip,
                                    which class files do not carry.
  src/outline.rs                    Per-file package, imports, and declared classes, including
                                    local and anonymous ones. Unchanged by edits inside a method.
  src/lower.rs                      Lowers a parsed file into declarations with resolved types.
  src/resolve.rs                    Resolves a type name to a class in the order the language
                                    specifies, and lowers type syntax into types.
  src/scope.rs                      What is in scope at an offset, and what a simple name means.
                                    Also gives untyped lambda parameters their target's types.
  src/members.rs                    Member lookup across a hierarchy with type arguments carried
                                    through; subtype tests; functional interface methods.
  src/overload.rs                   Assignability, the three applicability phases, most-specific
                                    selection, and generic method argument inference.
  src/solve.rs                      Solves a generic method's type variables from arguments, the expected
                                    type, and lambda results; supertype views and common supertypes.
  src/infer.rs                      Expression type inference, call resolution, and target typing,
                                    with cycle and depth guards.
  src/javadoc.rs                    Extracts a declaration's javadoc and splits its block tags.
  src/db.rs                         The analysis database: contents, outlines, index, caches, and
                                    the incremental invalidation rules.
  src/fixture.rs                    Builds a database from in-memory files for tests; behind the
                                    `fixture` feature so it is absent from release builds.
  tests/generics.rs                 Generic method inference: varargs, wildcards, shadowed type
                                    variables, lambdas, method references, nested calls, diamonds.
  tests/inference.rs                Inference, generics, overloads, inheritance, the acceptance
                                    cases from the brief, and that unresolvable code stays
                                    unresolved.

crates/java-ide/                    The features an editor asks for. Knows nothing of transports.
  src/lib.rs                        Crate root.
  src/convert.rs                    Byte offsets to LSP positions, ranges, and locations.
  src/uri.rs                        file: URI and path conversion, written here to avoid pulling
                                    in IDNA tables.
  src/render.rs                     Renders declarations and javadoc as the text an editor shows.
  src/completion.rs                 Classifies the cursor, gathers and ranks candidates, and
                                    repairs a collapsed parse when one blocks completion.
  src/hover.rs                      The declaration under the cursor, where it lives, its docs.
  src/target.rs                     Identifies which declaration a cursor refers to; shared by
                                    definition, references, rename, highlights, and tokens.
  src/navigation.rs                 Definition, type definition, implementations, references, and
                                    highlights, filtered textually then confirmed by resolution.
  src/rename.rs                     Workspace-wide rename on the same reference search.
  src/symbols.rs                    Document symbols, workspace symbols, selection ranges.
  src/diagnostics.rs                Syntax and semantic diagnostics, reported only where the
                                    analysis is complete.
  src/signature.rs                  Signature help: which overload, and which parameter.
  src/code_actions.rs               Quick fixes, organize imports, and generation of constructors,
                                    accessors, and overrides.
  src/imports.rs                    Adding an import in sorted position; organizing the block.
  src/tokens.rs                     Semantic tokens, classified by what each name resolves to.
  tests/features.rs                 One or more tests per feature, asserting what an editor shows.

crates/ferrisjdt/                    The server binary.
  src/main.rs                       Arguments, stderr-only logging, and the --self-check entry.
  src/server.rs                     The main loop: messages first, then indexing and diagnostics
                                    at quiet points, then block.
  src/state.rs                      Database, pending index queue, dirty files, cancelled
                                    requests, client capabilities.
  src/handlers.rs                   Request and notification dispatch.
  src/capabilities.rs               What the server advertises, only where a handler is useful.
  src/status.rs                     ferrisjdt/status: index and cache counters. Optional; no
                                    standard feature depends on it.
  src/selfcheck.rs                  Indexes a project and reports findings with timings.
  src/memory.rs                     Resident set size from the operating system.
  tests/protocol.rs                 Drives the real binary over stdio: handshake, incremental
                                    sync, each feature, and that stdout carries only protocol.
  tests/corpus.rs                   Analyses a thousand files of third-party Java and asserts
                                    nothing crashes or reports an impossible range.

crates/bench/                       Measurement.
  src/main.rs                       Drives any stdio language server through the same requests and
                                    reports latency percentiles, resident memory, and idle CPU.
                                    Used for both FerrisJDT and JDTLS.

tests/fixtures/                     Java corpora for tests and benchmarks.
  library/src/library/Genre.java    An enum for a book's genre.
  library/src/library/Book.java     A book: title, author, ISBN, genre, and checked-out state.
  library/src/library/Member.java   An abstract library member with an id, a name, and a
                                    checkout limit.
  library/src/library/Student.java  A member type with a declared major.
  library/src/library/Faculty.java  A member type with a declared department.
  library/src/library/Checkout.java A record of a book on loan to a member, with a due day.
  library/src/library/Library.java  Adds books and members, checks books in and out, and sorts
                                    and filters with a lambda and a method reference.
  library/src/library/LibraryUtils.java  Static helpers: a per-day fine and a formatted book
                                    listing.
  library/src/library/Main.java     A driver exercising every class. The whole directory
                                    compiles cleanly under javac -source 8, which is the baseline
                                    the "no false diagnostics" claim is measured against.
  probe.java                        One file exercising a wide spread of Java 8 syntax, used while
                                    developing against the grammar. Not a compilable program.
```

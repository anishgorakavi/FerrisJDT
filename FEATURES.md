# Features

A feature is listed as **implemented** only when it produces correct, useful
results *and* has automated tests. Where a feature works for the common cases
but has known gaps, it is listed as **partial** and the gaps are named.

Test names below are the ones that cover each feature; run `cargo test` to see
them.

## Protocol

| Request | State | Notes |
| --- | --- | --- |
| `initialize` / `initialized` | implemented | Capabilities negotiated: snippets, `LocationLink`, prepare-rename. `tests/protocol.rs` |
| `shutdown` / `exit` | implemented | `protocol.rs::stdout_carries_only_protocol_even_with_logging_on` |
| `textDocument/didOpen` | implemented | |
| `textDocument/didChange` | implemented | Incremental. Verified against a fresh parse character by character: `parsing.rs::incremental_edits_survive_a_long_typing_session` |
| `textDocument/didClose` | implemented | Falls back to the file on disk. |
| `textDocument/didSave` | implemented | |
| `workspace/didChangeWatchedFiles` | implemented | Create, change, and delete. |
| `$/cancelRequest` | implemented | Checked before analysis begins. |
| `ferrisjdt/status` | implemented | Server-specific; index and cache counters. Nothing standard depends on it. |

## Priority 0 — the semantic core

| Feature | State | Notes |
| --- | --- | --- |
| Completion: local variables | implemented | Respects declaration order: a local is not offered before its own declaration. |
| Completion: parameters | implemented | |
| Completion: fields | implemented | |
| Completion: methods | implemented | With a snippet of named parameters where the client supports it. |
| Completion: constructors | implemented | `new Foo` inserts a diamond for a generic class. |
| Completion: inherited members | implemented | Across files and through the classpath. |
| Completion: static members | implemented | `Integer.` offers statics and not instance methods. |
| Completion: standard library | implemented | From the configured JDK's own class files. |
| Completion: generic type parameters | implemented | `E` is offered, and a receiver of type `E` offers its bound's members. |
| Completion: generic substitution | implemented | `List<String>` offers `add(String)`, `get(): String`, `iterator(): Iterator<String>`. |
| Completion: relevant imports | implemented | An unimported type comes with an import edit in sorted position. |
| Completion: context-aware filtering | implemented | Member access, bare name, type position, `new`, and inside an `import`. |
| Completion: signatures and documentation | implemented | Parameter list, return type, owning class, and javadoc. |
| `completionItem/resolve` | partial | Fills in a type's javadoc when the list omitted it. The list already carries detail and documentation for members, so resolve rarely has work to do. |
| Hover | implemented | At a call site, the signature with the receiver's type arguments applied (`add(String e)`) plus the declaration it came from (`add(E e)`), the owner, the source file or archive, and rendered javadoc with `@param`/`@return`. |
| Go to definition | implemented | Locals, parameters, fields, methods, types, across files. Returns nothing for a classpath declaration, which has no source. |
| Find references | implemented | Textually filtered, then confirmed by resolution: a same-named field in another class is not a reference, and overloads stay distinct. |
| Document symbols | implemented | Hierarchical, with members and nested classes. |
| Publish diagnostics | implemented | See the diagnostics table below. |

## Priority 1 — expected IDE features

| Feature | State | Notes |
| --- | --- | --- |
| Signature help | implemented | Active parameter from comma position; the matching overload is listed first; works for constructors. |
| Rename symbol across files | implemented | Refuses invalid identifiers and classpath declarations, with a reason. |
| Workspace symbols | implemented | Classes, methods, and fields by name. |
| Go to implementation | implemented | Subtypes of a type, overrides of a method. Workspace only. |
| Type definition | implemented | Through arrays and type variables to the underlying class. |
| Code actions: add missing import | implemented | Candidates ranked with `java.util`, `java.lang`, `java.io` first. |
| Code actions: remove unused import | implemented | |
| Organize imports | implemented | Sorts, de-duplicates, drops unused, keeps wildcards. |
| Document highlights | implemented | Same resolution as references, restricted to one file. |
| Semantic tokens | implemented | Classified by resolution, so a field and a local differ. Full-document only. |
| Selection ranges | implemented | The chain of enclosing syntax nodes. |
| Basic formatting | **not implemented** | No formatter. Use an external one; `conform.nvim` with `google-java-format` works well alongside this server. |

## Priority 2 — generation and build integration

| Feature | State | Notes |
| --- | --- | --- |
| Generate constructor from fields | implemented | |
| Generate getters and setters | implemented | `isX` for a boolean; skips what already exists; no setter for a final field. |
| Override and implement methods | implemented | Inherited abstract methods, stubbed to throw rather than return a made-up value. |
| Extract variable | **not implemented** | |
| Extract method | **not implemented** | |
| Maven and Gradle dependencies | partial | The server reads JARs listed in `.ferrisjdt/classpath.txt`, which a client writes from `mvn dependency:build-classpath` or Gradle (the Neovim config does this). The server itself never runs a build tool. Tested by `reads_level_and_cached_classpath_from_a_gradle_project`. |
| Java level from the build file | implemented | `java.version`, `maven.compiler.*`, `JavaLanguageVersion.of`, `sourceCompatibility` when `sourceLevel` is unset. Tested by `explicit_level_beats_the_pom`. |
| Lombok | partial | `@Getter`, `@Setter`, `@Data`, `@Value`, `@With`, `@Accessors` (chain, fluent, prefix), `@NoArgsConstructor`/`@AllArgsConstructor`/`@RequiredArgsConstructor` (including `staticName`), `@Builder` (`toBuilder`, `@Singular`), `@SuperBuilder` (inheriting the parent's builder), `@FieldDefaults`, and the logger annotations (`@Slf4j` and friends). Generated members appear in completion, hover and diagnostics, and go-to-definition goes to the field. A method the class declares itself suppresses the generated one. Only active when the file imports `lombok`. Tested in `crates/java-ide/tests/lombok.rs`. |
| Multi-module projects | partial | Opening a module whose parent `pom.xml` lists it in `<modules>`, or whose parent `settings.gradle` includes it, makes the parent the workspace, so sibling modules' sources are indexed. Tested by `opening_one_maven_module_indexes_its_siblings` and the Gradle equivalent. |
| JUnit test discovery | **not implemented** | |

## Java language support

| Construct | State |
| --- | --- |
| Records | implemented — component fields, accessors, canonical and compact constructors, generic and varargs records. `records_have_components_accessors_and_a_canonical_constructor` |
| Pattern matching | implemented — `instanceof` type and record patterns, `case` patterns with guards, `var` components. Bindings are visible from the pattern to the end of the enclosing statement or block, not by definite assignment. `pattern_bindings_are_in_scope` |
| Enums | implemented — implicit `values()` and `valueOf(String)`, constants with bodies |
| try-with-resources variables | implemented — typed resources and `var`; `try_with_resources_variables_are_in_scope_in_the_body` |
| Qualified inner creation `outer.new Inner()` | implemented — `qualified_inner_class_creation_resolves_against_the_outer_instance` |
| Classes, interfaces, enums, annotations | implemented |
| Fields, methods, constructors, initialisers | implemented |
| Packages and imports, including static and on-demand | implemented |
| Inheritance and interface default methods | implemented |
| Method overriding and overload resolution | implemented |
| Primitive types, widening, boxing and unboxing | implemented |
| Arrays, including `length` and multi-dimensional | implemented |
| Generic classes, methods, and substitution | implemented |
| Wildcards and bounded type parameters | implemented |
| Type erasure | implemented |
| Raw types | implemented |
| Nested, inner, local, and anonymous classes | implemented |
| `this`, `super`, and qualified `this` | implemented |
| Lambdas and method references | partial — parameter types are inferred from the target functional interface; a target that must itself be inferred is not. See LIMITATIONS.md |
| Checked exceptions | partial — `throws` clauses are recorded, shown on hover, and resolved; whether a call's checked exceptions are handled is not verified |
| Enhanced `for`, `try`-with-resources, `switch`, labels | implemented |
| Static and instance initialisers | partial — analysed as a static or instance context, but not for definite assignment |

### Lambdas and method references

A lambda's parameter types come from the single abstract method of the
functional interface it targets. The target is found from a declared variable
type, a cast, an assignment, a `return`, or the parameter type of the call the
lambda is an argument to — so `(a, b) -> a.compareTo(b)` assigned to a
`Comparator<String>` gives both parameters `String`, and so does
`names.forEach(name -> ...)` on a `List<String>`. Where the target type must
itself be inferred, the parameters stay unknown and nothing is offered for
them. Inference never guesses: see LIMITATIONS.md.

## Diagnostics

| Diagnostic | Code | State |
| --- | --- | --- |
| Syntax errors | `syntax` | implemented |
| Unknown types | `unknown-type` | implemented, with an import suggestion |
| Undefined variables | `unknown-symbol` | implemented |
| Unknown members | `unknown-member` | implemented |
| Invalid method calls | `unknown-member` | implemented |
| Wrong argument count | `argument-count` | implemented, for methods and constructors |
| Argument type mismatches | `argument-type` | implemented; names the argument when one overload is in play |
| Incompatible assignments | `incompatible-types` | implemented, for local initialisers |
| Instantiating an abstract type | `incompatible-types` | implemented |
| Duplicate declarations | `duplicate-declaration` | implemented, for locals in one block |
| Invalid return types | `return-type` | implemented, including a missing return |
| Inaccessible members | `inaccessible` | partial — private fields and methods; other visibility rules are not checked |
| Missing imports | `unknown-type` | implemented |
| Unused imports | `unused-import` | implemented, as a hint |
| Language level violations | `language-level` | implemented — lambdas, `var`, records, text blocks, switch expressions |

Parser errors, semantic errors, and hints are distinguished by severity and by
code. Semantic checks are skipped inside any statement the parser recovered
in, and wherever a type did not resolve.

## Platforms

| Platform | State |
| --- | --- |
| macOS arm64 | implemented; developed and benchmarked here |
| Linux x86-64 | implemented; built and tested in CI |
| Windows | untested. The path and URI handling has a drive-letter case but nothing has run there. |

## Standard library sources

| Source | State |
| --- | --- |
| JDK 8 `rt.jar` | implemented |
| JDK 9+ `lib/ct.sym` | implemented, for the configured release |
| Extra JARs in `lib/` or from config | implemented |
| Class directories on the classpath | **not implemented** — JARs only |
| Javadoc and parameter names from `src.zip` | implemented — class files carry neither, so the JDK's own sources are parsed on demand to recover them. A JDK installed without `src.zip` falls back to `arg0`, `arg1` and no documentation. |

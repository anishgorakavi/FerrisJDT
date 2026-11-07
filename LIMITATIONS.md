# Limitations

What this server does not do, or does only partly. Nothing here is hidden
behind a feature that "returns a response": if a case is not handled, the
server either says so with a diagnostic or stays silent, and this file says
which.

## Semantic gaps

### Lambdas and method references

A lambda's parameter types come from the functional interface it targets. That
interface is found from a declared variable type, a cast, an assignment, a
`return`, or the parameter type of the call the lambda is an argument to, and
its single abstract method then gives the parameters their types:

```java
Comparator<String> byLength = (a, b) -> a.length() - b.length();  // a, b are String
names.forEach(name -> name.length());                             // name is String
Visitor<String> v = item -> item.length();                        // works for your own interfaces too
```

What is not implemented is a target type that must itself be inferred — a
lambda passed to a generic method whose type argument is only determined by
that same lambda:

```java
// `x`'s type depends on solving T from the lambda itself, which is not done.
list.stream().map(x -> x.length());
```

There the parameter stays unknown. The body is still parsed and navigable, and
no false diagnostics are produced: inference returns unknown and every check
stands down. A method reference (`String::length`, `String[]::new`) is
resolved only as far as its target type is known, which is the same
restriction.

### Generic method type inference

Argument-driven inference is implemented for the shapes that occur in ordinary
code — `Arrays.asList("a")`, `Collections.sort(list)`, `map.put(k, v)` — by
matching each parameter against its argument. What is not implemented is the
constraint-solving of JLS 18: inference from the return type's context, nested
inference through generic arguments, and bounds that interact. A variable that
cannot be solved falls back to its declared bound, which is what erasure would
give, so the result is usable rather than unknown — but it can be less precise
than `javac` would be.

### Checked exceptions

`throws` clauses are parsed, resolved, shown on hover, and carried on every
method. Whether a call's checked exceptions are caught or declared is **not**
verified, so forgetting to handle an `IOException` is not reported.

### Definite assignment and reachability

Not implemented. A final field assigned on only one branch, a local read
before assignment, or unreachable code after a `return` are not reported.

The one exception is a non-void method containing no `return` at all, which is
reported, because that cannot compile for any reason other than always
throwing. Partial reachability analysis was deliberately not attempted: it
would report correct code.

### Accessibility

Private members — both fields and methods — are checked, and a member that
exists but is not visible is reported as inaccessible rather than as missing.
What is *not* checked is the rest of the visibility rules: protected access
from an unrelated package, and package-private access across packages, are not
reported.

Completion filters by visibility in all cases, so an inaccessible member is
never offered; it is only the hand-written case that goes undiagnosed.

### Other known gaps

* **Overload resolution with generic receivers and varargs together** can pick
  a different overload than `javac` in unusual cases. The three applicability
  phases and most-specific selection are implemented; the interaction with
  inference variables is simplified.
* **Intersection and union types** appear only where the syntax forces them
  (a multi-catch, a type parameter with several bounds). An intersection is
  treated as its first member for member lookup, so a member coming only from
  a later bound is found, but one requiring the intersection as a whole is not
  modelled.
* **Annotations** are parsed and `@Deprecated` and `@Override` are recognised.
  Annotation *values* are not evaluated, and no annotation processing happens.
* **`java.lang.Object` methods on interfaces** are offered on a value of
  interface type, which is correct, but `getClass()` returns
  `Class<?>` rather than `Class<? extends |T|>`.
* **Static nested class type parameters** are handled, but a *non-static*
  inner class's reference to the outer class's parameters is resolved by name,
  which is right in practice and would be wrong if an inner class shadowed an
  outer parameter with a different meaning.

## Project model gaps

* **Maven and Gradle dependencies are not resolved.** The standard source
  layouts (`src/main/java`, `src/test/java`) are found and indexed, but
  `pom.xml` and `build.gradle` are not read. A project with third-party
  dependencies will report their types as unknown until the JARs are listed in
  `ferrisjdt.json` or dropped in `lib/`. This is the largest functional
  difference from JDTLS.
* **Class directories on the classpath are not supported** — JARs only. A
  dependency that is an unpacked `target/classes` has to be jarred.
* **Multi-module projects** are indexed as one flat source set. Two modules
  that declare the same fully qualified class will collide, with the first
  indexed winning.
* **No build or test running.** No JUnit discovery, no debugger, no
  `javac` invocation.

## Performance characteristics

* **The first request after an edit re-lowers the edited file.** For a
  course-sized file that is about 3 ms; for a 24 KiB file with 60 members it
  is about 31 ms, which is what makes go-to-definition slower than JDTLS on a
  large file. Lowering is per file, not per class, so a one-character edit
  redoes the whole file's declarations. Making it per class is the clearest
  remaining improvement.
* **Adding, removing, or renaming a class invalidates every file's lowered
  declarations.** Typing inside a method body does not, which is the case that
  matters for responsiveness, but the first request after a structural edit in
  a large workspace pays to re-lower what it touches.
* **Workspace-wide operations scale with the workspace.** References and
  rename filter files textually first, so a name that appears in one file is
  cheap; a name like `get` that appears everywhere costs a resolution pass per
  occurrence.
* **Initial indexing is proportional to source size** — about 1 second per
  6.7 MiB. The server answers `initialize` immediately and indexes in the
  background, but a request arriving before indexing finishes waits for it,
  because answering from a half-built index would be wrong rather than late.
* **Very large single files are slow to diagnose.** A 155 KiB file in which
  nothing resolves took 801 ms. The server diagnoses only open files, and only
  at quiet points, so this is felt as a delay before diagnostics appear rather
  than as input lag.

## Protocol gaps

* **No formatting.** `textDocument/formatting` is not implemented and not
  advertised. Use an external formatter.
* **No inlay hints, no code lens, no call hierarchy, no type hierarchy, no
  folding ranges, no linked editing.**
* **Semantic tokens are full-document only**; no range or delta requests.
* **No work-done progress** during indexing. The server does not report
  progress, so an editor shows nothing while a large workspace is indexed.
* **No pull diagnostics** (`textDocument/diagnostic`); diagnostics are pushed.
* **Workspace symbol results are capped** at 256, and classpath classes are
  not included — only workspace declarations.
* **Completion caps importable classpath types** at 200 per request and sets
  `isIncomplete`, so the client re-queries as the prefix grows.

## Platform

* **Windows is untested.** Path and URI handling has a drive-letter case, but
  nothing has been run there.
* **Resident memory reporting** (`--self-check`, `ferrisjdt/status`) works on
  macOS and Linux and returns nothing elsewhere.

## Standard library

* **Parameter names and javadoc come from the JDK's `src.zip`.** A JDK
  installed without it falls back to positional names (`arg0`, `arg1`) and no
  documentation for standard library members. Project code is unaffected.
* **A JDK 9+ installation is read through `ct.sym`**, which carries
  declarations for the configured release but no javadoc or parameter names
  unless `src.zip` is also present.
* **Only one JDK is used.** There is no per-module language level or
  multi-release JAR handling.

## Testing gaps

* **Differential testing against `javac` is manual.** The fixture project is
  compiled by `javac` in CI, which guarantees the "no false positives"
  baseline is real, but there is no automated comparison of individual
  diagnostic messages across a corpus.
* **No comparison against JDTLS's completion ranking.** Semantic correctness
  of completion is tested; ordering is not, and is not intended to match.
* **No fuzzing harness is committed.** Malformed input is covered by
  hand-written adversarial cases — truncated and bit-flipped class files, every
  prefix of a real class file, malformed archives, deeply nested and
  unterminated source — rather than by a fuzzer. A `cargo-fuzz` target for the
  class-file reader and the signature parser is the obvious next step.

## Next steps, in order of value

1. **Lower declarations per class rather than per file.** This is the single
   change behind the weakest number in the benchmarks: the first request after
   an edit currently redoes the whole file's declarations, which is what makes
   go-to-definition cost 31 ms on a 24 KiB file instead of 3 ms.
2. **Read Maven and Gradle dependencies.** Not by invoking the build tool on
   every start, but by parsing `pom.xml` and the Gradle cache for JAR paths,
   with an optional one-off `mvn dependency:build-classpath` behind a setting.
   This is the largest functional gap against JDTLS.
3. **Solve lambda target types through generic method inference**, which would
   complete `list.stream().map(x -> ...)`.
4. **Formatting**, so the server is self-sufficient.
5. **A `cargo-fuzz` target** for the class-file reader, the signature parser,
   and the completion entry point, replacing the hand-written adversarial
   cases with continuous coverage.

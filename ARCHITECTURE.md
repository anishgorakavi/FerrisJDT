# Architecture

FerrisJDT answers editor questions about Java source without a JVM. It parses
with tree-sitter, reads the JDK's own class files directly, and builds the
semantic layer — scopes, types, substitution, overload resolution — itself.

```
                   ┌──────────────┐
   stdin/stdout ───│   ferrisjdt   │  JSON-RPC, dispatch, main loop
                   └──────┬───────┘
                          │
                   ┌──────┴───────┐
                   │   java-ide   │  completion, hover, navigation,
                   └──────┬───────┘  diagnostics, rename, tokens
                          │
                   ┌──────┴───────────┐
                   │  java-semantic   │  Db: index, scopes, types,
                   └──┬────────┬──────┘  members, inference
                      │        │
        ┌─────────────┘        └──────────────┐
┌───────┴───────┐                    ┌────────┴────────┐
│  java-syntax  │                    │ java-classfile  │
│  tree-sitter, │                    │  .class / JAR   │
│  positions    │                    │  decoding       │
└───────────────┘                    └────────┬────────┘
                                     ┌────────┴────────┐
                                     │  project-model  │
                                     │ roots, JDK, cp  │
                                     └─────────────────┘
```

Dependencies point one way only. `java-syntax` knows nothing of types;
`java-classfile` knows nothing of classpaths; `java-semantic` knows nothing of
LSP; `java-ide` knows nothing of transports.

## Why these crates

The suggested layout in the brief named eight. Six carry their weight:

* `java-index` is not separate from `java-semantic`. The index *is* the
  semantic database — it holds file contents, outlines, the name-to-file map,
  and the caches, and the invalidation rules tie them together. Splitting it
  would mean exposing the cache internals across a crate boundary for no gain.
* `java-completion` and `java-diagnostics` are modules of `java-ide`. They
  share the same conversion, rendering, and target-identification code, and a
  crate per LSP request would multiply boundaries without reducing coupling.

## The type model

A `Type` is a plain owned tree. A class type names its class rather than
pointing at it:

```rust
Type::Class { name: Name, args: Vec<Type> }
```

Looking a name up is the database's job. This matters more than it looks:
Java's recursive and F-bounded declarations — `class Node { Node next; }`,
`<E extends Comparable<? super E>>` — would form reference cycles if a type
held its class, and reference-counted cycles leak. With names, the type of
`root` is just `Class("ds.BSTNode", [Var("E")])` and nothing is cyclic.

A type variable's bounds are carried with it, but the variables *inside* a
bound carry none. That keeps `E extends Comparable<? super E>` a finite tree
rather than an infinite one, and the places that need a bound's bound ask the
database instead. A type variable's identity is its name and owner, not its
bounds, so the `E` inside the bound and the `E` of the class compare equal.

Types are cloned by value rather than shared. For the sizes that occur in
Java — a handful of arguments, nesting two or three deep — that is cheaper
than reference counting, and it keeps substitution a pure function.

## Generic substitution

This is what makes completion useful rather than decorative. Looking up `add`
on `ArrayList<String>`:

1. `ArrayList<E>` binds `E := String`.
2. Its superclass reference is `AbstractList<E>`; applying the substitution
   gives `AbstractList<String>`, which binds that class's own `E := String`.
3. Its interface `List<E>` likewise becomes `List<String>`.
4. `List.add(E)` is found at that point, and the composed substitution turns
   it into `add(String)`.

Each step composes the substitution so far with the one the supertype
reference introduces. A raw use of a generic class — `List x` rather than
`List<String> x` — substitutes each variable with its erasure, so a raw
receiver still yields `get(): Object` instead of an unresolved variable.

Overriding is resolved by erased signature, which is what the language
compares, so a subclass's `add` hides the inherited one exactly as it should.

## Reading the JDK without a JVM

A JDK 8 exposes its whole library as `jre/lib/rt.jar`, an ordinary JAR of
class files. JDK 9 and later replaced it with a container that is not a ZIP,
but they ship `lib/ct.sym` — the archive `javac --release` reads — whose
entries are per-release class files in a ZIP. Either is read the same way.

Only the directory is read at startup: 20,390 class names for `rt.jar`, held
as one string arena plus a fixed-size record each, about a megabyte. Classes
are decoded when a request needs them, and the decoded form is cached under a
bound. Generic `Signature` attributes are parsed, which is what preserves
`ArrayList<E> implements List<E>` and `E get(int)`.

The ZIP reader is hand-written rather than taken from a crate. The reason is
memory: a general-purpose reader's in-memory directory costs several megabytes
for 20,000 entries, which would dominate the budget.

## Incremental analysis

Four facts, each invalidated only by its own inputs:

| What | Depends on | Invalidated when |
| --- | --- | --- |
| Text and version | the editor | every `didChange` |
| Parse tree | text | every `didChange`, incrementally |
| Outline (package, imports, declared classes) | text | every `didChange`, but compared for shape |
| Lowered `ClassData` | the file's text, and the set of classes in the workspace | the file changes, or `outline_revision` moves |

`outline_revision` increases only when some file's outline changes *shape* —
a class added, removed, or renamed. Typing inside a method body leaves it
alone, so nothing outside the edited file is invalidated. That is the whole
reason there is no full reindex per keystroke.

Parse trees are kept for open documents and in a small bounded cache for the
rest; a closed file's tree is rebuilt from its text in about a millisecond,
and holding every tree would cost many times the size of the sources. This was
measured: retaining one tree per file put a 1,010-file project at 175 MiB,
against 29 MiB without.

### Surviving a broken parse

Error recovery sometimes collapses a whole class. A bare `this.` is enough:
the parser unwinds the class declaration entirely, and for one keystroke the
file appears to declare nothing. Without care, the class would vanish from the
index, taking completion inside it and every cross-file reference to it along.

So each file also retains the text and outline of its most recent parse that
*did* yield all its declarations. Resolution reads that; anything producing
ranges in the live document reads the current parse. Only the text is
retained, not the tree, so this costs nothing in the common case — it is the
same `Arc` — and the retained text is re-parsed only while a file is
transiently broken.

A file can also be *saved* in that state, leaving no earlier parse to fall
back on. Completion then repairs the document by inserting a placeholder
identifier at the cursor, analyses the result, and restores the original. That
is a last resort, costing two extra parses, and it runs only when the cursor
is not inside any class the resolver can see.

## The main loop

One thread owns the database outright. No locks, no shared mutable state, and
no chance of a request observing a half-applied edit.

Responsiveness comes from ordering instead:

1. A queued message is always handled first.
2. When the queue is empty, index the next few files.
3. When the queue is empty and indexing is done, publish diagnostics for files
   that changed. A burst of keystrokes therefore produces one diagnostics pass,
   not one per key — the debounce is the quiet point itself, with no timer.
4. Otherwise block on the channel, so an idle server uses no CPU.

A request that arrives while indexing finishes the index first. Answering from
a half-built index would produce a missing completion or a definition that
goes nowhere — confidently wrong rather than merely late.

Cancelled requests are dropped before any analysis runs, which is where a
queued cancellation can still save the work.

## Honesty about what is unknown

An expression whose type cannot be determined gets `Type::Unknown`, and that
value propagates. Every consumer treats it as a signal to stay quiet:
completion offers nothing for an unresolvable receiver, and a diagnostic is
only reported when every type involved resolved. Nothing inside a region the
parser had to recover from is checked at all, because the tree there does not
reflect what the author wrote.

The practical test of this is the fixture project: `javac` compiles it
cleanly, and the server reports zero diagnostics on it. The same code reports
all twelve errors in a deliberately broken file, matching `javac` on each one.

## Bounds on untrusted input

Project files and JARs are untrusted. Every recursive walk carries a visited
set and a depth bound; the class-file and ZIP readers return errors rather
than panicking; the source walk refuses symlinks and stops at fixed depth and
file counts; decompression refuses anything over 64 MiB; and the caches for
decoded classes, parse trees, and lowered files all have ceilings.

Expression inference needs one bound beyond depth. A call's type depends on
its arguments, and an argument's type can depend on the call — that is how a
lambda or `String[]::new` gets its target type. The cycle is genuine, and a
depth counter alone does not break it because each turn starts a fresh
inference. Calls in progress are tracked by node identity and not re-entered.
That bug was found by analysing the Eclipse JDT language server's own source,
where it exhausted the stack.

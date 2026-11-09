# FerrisJDT

A lightweight Java language server, written in Rust. It replaces JDTLS for
everyday Java work — completion, hover, navigation, diagnostics, rename — on a
fraction of the memory and with no JVM of its own.

On a course-sized project it uses **17 MiB** against JDTLS's **1.29 GiB**, is
ready in **76 ms** against **2.2 s**, and answers completion at a p95 of
**0.7 ms** against **77 ms**. The numbers, the method, and the cases where
JDTLS is faster are in [BENCHMARKS.md](BENCHMARKS.md).

It is built for Java 8 source, and specifically for undergraduate data
structures work: generic collections, `Comparable` and `Comparator`, trees and
nodes, and all the half-typed code that comes with writing them.

## What it does

```java
class BinarySearchTree<E extends Comparable<? super E>> {
    private BSTNode<E> root;

    public boolean add(E value) {
        root.          // offers data, left, right — the members of BSTNode<E>
        value.         // offers compareTo — from the bound Comparable<? super E>
    }
}
```

Both of those work because the server resolves types rather than matching
names. `List<String> names` offers `add(String)` and `get(): String`, read
from the JDK's own class files with their generic signatures intact.

See [FEATURES.md](FEATURES.md) for what is implemented, what is partial, and
what is not there — nothing is listed as working without tests behind it.

## Requirements

* A Rust toolchain, 1.75 or newer.
* A JDK for the standard library. A JDK 8 is read from `jre/lib/rt.jar`; a
  JDK 9 or newer is read from `lib/ct.sym`. No JVM is ever started.
* macOS arm64 or Linux x86-64. Windows is untested.

## Install

```sh
git clone <this repository>
cd ferrisjdt
make install            # builds --release and installs ferrisjdt and ferrisjdt-doctor to ~/.local/bin
```

Or by hand:

```sh
cargo build --release
install -m 755 target/release/ferrisjdt target/release/ferrisjdt-doctor ~/.local/bin/
```

Check it:

```sh
ferrisjdt --version
ferrisjdt --self-check path/to/your/java/project --verbose
```

`--self-check` indexes a project and reports what it found, with timings. It
is the quickest way to see whether your project resolves and where the JDK was
picked up from.

### Troubleshooting

If standard types such as `String` or `Comparable` are flagged as unrecognized
symbols, the server almost certainly did not find a JDK. Run the doctor:

```sh
ferrisjdt-doctor path/to/your/java/project           # human-readable report
ferrisjdt-doctor path/to/your/java/project --verbose # also list where JDKs were searched
ferrisjdt-doctor path/to/your/java/project --json    # for scripts and bug reports
```

It checks the server binary, `JAVA_HOME`, `ferrisjdt.json`, JDK discovery, the
standard library index, source files and extra JARs, and prints a fix for each
problem. It exits 1 when a check fails. It is built alongside the server
(`cargo build --release` puts both in `target/release`).

## Neovim

The server speaks ordinary LSP over stdin and stdout, so any client can start
it. With Neovim 0.11 or newer:

```lua
vim.lsp.config("ferrisjdt", {
  cmd = { "ferrisjdt" },
  filetypes = { "java" },
  root_markers = { "ferrisjdt.json", "pom.xml", "build.gradle", ".git", "src" },
  init_options = { sourceLevel = 8 },
})
vim.lsp.enable("ferrisjdt")
```

### LazyVim

LazyVim's `lang.java` extra sets up JDTLS through `nvim-jdtls`. To use this
server instead, add `lua/plugins/ferrisjdt.lua`:

```lua
return {
  -- JDTLS starts a JVM per project and is what this replaces, so its plugin
  -- is turned off rather than left to attach a second client to every buffer.
  { "mfussenegger/nvim-jdtls", enabled = false },

  {
    "neovim/nvim-lspconfig",
    opts = {
      servers = {
        -- `false` stops LazyVim from installing or enabling JDTLS.
        jdtls = false,

        ferrisjdt = {
          cmd = { vim.fn.exepath("ferrisjdt") },
          filetypes = { "java" },
          root_markers = {
            "ferrisjdt.json", "pom.xml", "build.gradle", "build.gradle.kts", ".git", "src",
          },
          init_options = { sourceLevel = 8 },
        },
      },
    },
  },
}
```

Nothing else needs changing: LazyVim passes `opts.servers.<name>` straight to
`vim.lsp.config` and enables any server Mason does not own, so a server it has
never heard of works as-is.

You can keep the `lang.java` extra enabled — it is still what brings in Java
treesitter and the Java debug adapter. Only `nvim-jdtls` is turned off.

To check that it attached, open a Java file and run `:checkhealth lsp` or
`:lua =vim.lsp.get_clients({ bufnr = 0 })[1].name`.

Formatting is not provided; this server has no formatter. `conform.nvim` with
`google-java-format` works well alongside it.

## Project configuration

Nothing is required: a directory of `.java` files works, as does a Maven or
Gradle source layout. The server does not run Maven or Gradle. It reads the
dependency JARs listed one per line in `.ferrisjdt/classpath.txt`, and a client
writes that file: Neovim does it with `:JavaDepsSync` (see its `javadeps.lua`),
or run `mvn dependency:build-classpath` yourself. JARs in `lib/` are also picked
up automatically, and the Java level comes from `pom.xml` or `build.gradle` when
`sourceLevel` is not set.

Settings come from `initializationOptions`, or from a `ferrisjdt.json` at the
project root. Client settings win.

```json
{
  "sourceLevel": 8,
  "jdkHome": "/Library/Java/JavaVirtualMachines/corretto-1.8.0_504/Contents/Home",
  "sourceRoots": ["src", "test"],
  "classpath": ["lib/junit-4.13.2.jar", "vendor"],
  "exclude": ["generated"],
  "maxSourceFiles": 20000
}
```

| Key | Meaning |
| --- | --- |
| `sourceLevel` | Java language level, as a number. Default: the level in `pom.xml` or `build.gradle`, else 8. Constructs newer than this are reported. |
| `jdkHome` | JDK to read the standard library from. Discovered when absent, preferring one whose release matches `sourceLevel`. |
| `sourceRoots` | Extra source directories, relative to the project root or absolute. |
| `classpath` | Extra JARs, or directories to collect JARs from. |
| `exclude` | Directory names never descended into, on top of the defaults (`target`, `build`, `out`, `bin`, `node_modules`, and the dot-directories). |
| `maxSourceFiles` | Ceiling on indexed source files. Default 20,000. |

## Usage

```
ferrisjdt [--stdio]              run as a language server over stdin/stdout
ferrisjdt --self-check <dir>     index a project and report findings
ferrisjdt --version
ferrisjdt --help
```

`FERRISJDT_LOG` sets the log filter (`info`, `debug`, `ferrisjdt=trace`). Logs
always go to stderr — stdout carries the protocol and nothing else, which
`tests/protocol.rs` enforces.

## Development

```sh
make            # release build and tests
make test       # debug tests
make check      # format, clippy, release tests
make selfcheck  # index the fixture project and report
make bench      # latency and memory, FerrisJDT
make bench-jdtls  # the same harness against JDTLS, for comparison
```

The test suite is 116 tests across unit, integration, protocol, and corpus
levels. The protocol tests spawn the real binary and drive it over
stdio; the corpus tests analyse a thousand files of third-party Java and
assert nothing crashes. CI additionally compiles the fixture project with
`javac`, because the claim that the server reports no false diagnostics on it
is only meaningful against a baseline that really compiles.

## Documentation

| | |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Components, the type model, generic substitution, incremental analysis |
| [TREE.md](TREE.md) | Every source file and what it is responsible for |
| [FEATURES.md](FEATURES.md) | Implemented, partial, and unsupported, with the tests that cover each |
| [BENCHMARKS.md](BENCHMARKS.md) | Measurements, methodology, and how to reproduce them |
| [LIMITATIONS.md](LIMITATIONS.md) | Semantic gaps, project model gaps, performance characteristics |

## Licence

MIT.

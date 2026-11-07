# Benchmarks

Every number here was produced by `crates/bench`, which drives a language
server over stdio and measures what an editor would experience. FerrisJDT and
JDTLS were measured by the same harness, with the same requests, in the same
order, on the same machine. The commands are below; re-running them should
reproduce the figures within noise.

## Hardware and software

| | |
| --- | --- |
| Machine | Apple M4, 10 cores, 16 GiB |
| OS | macOS 27.2 (arm64) |
| Rust | 1.99.1, `--release` (opt-level 3, thin LTO) |
| JDK for analysis | Amazon Corretto 1.8.0_504, arm64 (`jre/lib/rt.jar`, 66 MiB, 20,390 classes) |
| JDTLS | Eclipse JDT Language Server 1.63 (mason), on OpenJDK 25.0.5 |
| Measured | 2026-11-07 |

## Corpora

| Name | Size | What it is |
| --- | --- | --- |
| `library` | 9 files, 6.5 KiB, 9 types | `tests/fixtures/library`: an intro-course-style project — a `Book`, an abstract `Member` with `Student` and `Faculty` subtypes, a `Checkout` record, a `Genre` enum, a `Library` that manages checkouts using a lambda and a method reference, and a driver. Compiles cleanly under `javac -source 8`. |
| `eclipse.jdt.ls` | 1,010 files, 6.7 MiB, 1,150 types | The Eclipse JDT language server's own source tree, checked out beside this repository. Real third-party Java: generics-heavy, annotation-heavy, with inner and anonymous classes throughout. |

A note on fairness for the medium corpus: `eclipse.jdt.ls` is a Maven/Tycho
project. JDTLS resolves its dependencies, which is most of its 202-second
readiness time and most of its memory; FerrisJDT does not read `pom.xml` at
all, so it analyses the same sources with the Eclipse platform types
unresolved. The latency and memory figures are comparable; the readiness
figures are not measuring the same work, and that is called out below rather
than papered over.

## Methodology

* **Cold start** is the wall time from sending `initialize` to receiving its
  response, with the process already spawned.
* **Ready** is from `didOpen` to the first `publishDiagnostics` for that file.
* **Latency** samples are warm: five unmeasured requests run first, then 50
  measured iterations (20 for JDTLS on the medium corpus). Percentiles are
  nearest-rank, with no interpolation.
* **Completion** is measured *after a keystroke*, not on a resting document: a
  `didChange` inserts `receiver.` and completion is requested at the caret it
  created. Measuring a resting document would flatter any server that caches
  per document version.
* **Edit → diagnostics** is from sending a `didChange` to receiving the
  diagnostics that follow it.
* **Memory** is resident set size from `ps -o rss`, summed over the server
  process and every descendant, because a JVM-based server does its work in a
  child. Virtual size and heap accounting are both excluded as misleading.
  Two figures are given: directly after the measured workload, and after the
  process has been idle for 4.5 seconds — on macOS the second can be much
  lower, because the system compresses pages the server is no longer touching.
  **The "after workload" figure is the one to compare.**
* **Idle CPU** is CPU-seconds consumed over a 3-second window with no traffic.
* All figures are warm-cache: the binaries and the JDK are in the OS page
  cache. The first ever run of a freshly built binary measured 254 ms to cold
  start rather than 3 ms, entirely in page faults.

## Results

### Small project (`library`, 9 files)

| Metric | FerrisJDT | JDTLS 1.63 | Ratio |
| --- | --- | --- | --- |
| Cold start | **3.3 ms** | 2,605 ms | 789× faster |
| Ready (first diagnostics) | **79.1 ms** | 2,852 ms | 36× faster |
| Resident after workload | **17.0 MiB** | 1,317.7 MiB | **77× less** |
| Resident when idle | 17.0 MiB | 1,234.9 MiB | 73× less |
| Idle CPU | 0.0 % | 0.0 % | — |
| Completion p50 / p95 | **0.51 / 0.64 ms** | 10.49 / 125.29 ms | 196× faster at p95 |
| Hover p50 / p95 | **0.28 / 0.31 ms** | 2.84 / 12.24 ms | 39× faster at p95 |
| Definition p50 / p95 | **1.69 / 1.81 ms** | 2.42 / 13.86 ms | 7.7× faster at p95 |
| Document symbols p95 | **0.11 ms** | 7.63 ms | 69× faster |
| Edit → diagnostics p95 | **1.78 ms** | 502.70 ms | 282× faster |

### Medium project (`eclipse.jdt.ls`, 1,010 files)

| Metric | FerrisJDT | JDTLS 1.63 | Ratio |
| --- | --- | --- | --- |
| Cold start | **3.7 ms** | 2,245 ms | 607× faster |
| Ready (first diagnostics) | **1.09 s** | 201.7 s | not comparable — see above |
| Resident after workload | **31.6 MiB** | 1,771.7 MiB | **56× less** |
| Resident when idle | 3.5 MiB | 1,774.0 MiB | — |
| Idle CPU | 0.0 % | 0.0 % | — |
| Completion p50 / p95 | **2.72 / 2.87 ms** | 28.25 / 104.81 ms | 37× faster at p95 |
| Hover p50 / p95 | **2.27 / 10.60 ms** | 1.93 / 6.01 ms | 0.6× (JDTLS faster) |
| Definition p50 / p95 | 30.98 / 31.95 ms | **1.47 / 7.54 ms** | 0.24× (JDTLS faster) |
| Document symbols p95 | **0.50 ms** | 20.86 ms | 42× faster |
| Edit → diagnostics p95 | **31.86 ms** | 518.93 ms | 16× faster |

### Against the stated targets

| Target | Small | Medium | Met |
| --- | --- | --- | --- |
| Idle resident, small project < 100 MB | 17.0 MiB | — | yes |
| Resident, medium project < 250 MB | — | 31.6 MiB | yes |
| Cold startup < 500 ms | 3.3 ms | 3.7 ms | yes |
| Warm completion p95 < 50 ms | 0.64 ms | 2.87 ms | yes |
| Hover p95 < 50 ms | 0.31 ms | 10.60 ms | yes |
| Go-to-definition p95 < 100 ms | 1.81 ms | 31.95 ms | yes |
| No full reindex per keystroke | yes | yes | yes |
| CPU near zero while idle | 0.0 % | 0.0 % | yes |

## Where JDTLS is faster, and why

JDTLS wins go-to-definition, and hover on the medium corpus. This is not
noise and it is not a measurement artefact: JDTLS has built a fully resolved
binding model for the whole project up front — which is what its 202 seconds
and 1.7 GiB bought — so a definition lookup is a hash lookup in an existing
structure.

FerrisJDT resolves on demand. Definition on the medium corpus costs about
31 ms because the first request after an edit re-lowers the edited file's
declarations, and the file in question is 24 KiB with 60 members. The same
operation on a course-sized file costs 1.7 ms. The trade is deliberate: a
constant 31 ms on a large file against three minutes of startup and 56× the
memory. It is also the clearest thing left to improve — see LIMITATIONS.md.

## Other measurements

### Indexing and analysis throughput

From `ferrisjdt --self-check`, which runs the same code paths without an
editor. Median of three runs:

| Corpus | Classpath index | Parse + index sources | Lower all types | Diagnostics, all files |
| --- | --- | --- | --- | --- |
| `library` (9 files, 6.5 KiB) | 42.6 ms, 20,390 classes | 1.7 ms | 1.7 ms, 9 types, 57 members | 34.7 ms |
| `eclipse.jdt.ls` (1,010 files, 6.7 MiB) | 42 ms, 20,390 classes | 1,030 ms | 1,050 ms, 1,397 types, 11,837 members | 11.2 s |

Diagnosing a whole workspace is not something the server ever does — it
diagnoses open files only — but it is the honest aggregate. Per file it is
about 11 ms on this corpus. Lowering is slower than it was before standard
library source attachment was added (about 360 ms on this corpus without it):
decoding a JDK class now also parses its source from `src.zip` to recover
parameter names and javadoc. That cost is paid once per class touched and is
cached, which is why it does not appear in the warm latency figures.

### Worst-case single file

| File | Size | Diagnostics |
| --- | --- | --- |
| `JavaLanguageServerPlugin.java` | 24 KiB | 48 ms |
| `CompletionHandlerTest.java` | 169 KiB | 55 ms |
| `Preferences.java` | 143 KiB | 152 ms |
| `UnresolvedMethodsQuickFixTest.java` | 155 KiB | 801 ms |

The last is the slowest file in the corpus and is an outlier: a 155 KiB test
file in which almost every type is unresolved, so no check can be skipped
early. A file that size is far outside course work, but it is the number to
beat, and `tests/corpus.rs` asserts that no file exceeds two seconds.

### How memory grows

Resident set size reported by `--self-check`, which is the heaviest thing the
server can be asked to do — it indexes, lowers *every* type, and diagnoses
*every* file:

| State | Resident |
| --- | --- |
| Classpath indexed, no sources (empty project directory) | 12.0 MiB |
| `library` fully indexed, lowered, and diagnosed | 16.5–16.6 MiB |
| `eclipse.jdt.ls` fully indexed, lowered, and diagnosed (1,010 files) | 64.5 MiB |

Under a real editor workload — one file open, 50 requests — the medium corpus
sits at 31.6 MiB, because only the files that were touched are lowered.

Retaining one parse tree per indexed file was measured at 175.2 MiB on the
medium corpus under that same editor workload, against 31.6 MiB when only
texts and outlines are kept and trees are rebuilt on demand. That measurement
is why the database works the way it does.

## Reproducing

```sh
# Build everything optimised.
cargo build --release

# Small project.
./target/release/ferrisjdt-bench \
  --project tests/fixtures/library \
  --target src/library/Library.java \
  --symbol member --receiver "book." --insert-line 30 --runs 50 \
  --label "FerrisJDT / library" --command "$PWD/target/release/ferrisjdt"

# Medium project: check out eclipse.jdt.ls beside this repository first.
./target/release/ferrisjdt-bench \
  --project ../eclipse.jdt.ls \
  --target org.eclipse.jdt.ls.core/src/org/eclipse/jdt/ls/core/internal/JavaLanguageServerPlugin.java \
  --symbol logHandler --receiver "logHandler." --insert-line 161 --runs 50 \
  --label "FerrisJDT / eclipse.jdt.ls" --command "$PWD/target/release/ferrisjdt"

# The same harness against JDTLS. Needs `jdtls` on PATH and a JDK new enough
# to run it; run it from a scratch directory, because it writes a data folder.
cd /tmp && mkdir -p jdtls-bench && cd jdtls-bench
JAVA_HOME=/opt/homebrew/opt/openjdk@25/libexec/openjdk.jdk/Contents/Home \
  <path>/ferrisjdt-bench --project <corpus> --target <file> --symbol <name> \
  --receiver "<expr>." --insert-line <n> --runs 50 --ready-timeout 720 \
  --label "JDTLS" --command jdtls

# Indexing throughput and a correctness pass over a project.
./target/release/ferrisjdt --self-check tests/fixtures/library --verbose
```

`make bench` runs the FerrisJDT suite; `make bench-jdtls` runs the comparison.

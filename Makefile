# Tabs are required by make itself; the rest of the project uses spaces.
PREFIX ?= $(HOME)/.local
BIN := $(PREFIX)/bin
CORPUS ?= ../eclipse.jdt.ls
BENCH := ./target/release/ferrisjdt-bench
SERVER := $(CURDIR)/target/release/ferrisjdt

.PHONY: all build release test test-release lint fmt check install uninstall bench bench-ferrisjdt bench-jdtls selfcheck clean

all: release test

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

# The corpus tests parse a thousand files, which is far quicker optimised.
test-release:
	cargo test --release

lint:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all

check: fmt lint test-release

install: release
	mkdir -p $(BIN)
	install -m 755 target/release/ferrisjdt $(BIN)/ferrisjdt
	install -m 755 target/release/ferrisjdt-doctor $(BIN)/ferrisjdt-doctor
	@echo "installed $(BIN)/ferrisjdt and $(BIN)/ferrisjdt-doctor"

uninstall:
	rm -f $(BIN)/ferrisjdt $(BIN)/ferrisjdt-doctor

# Index a project and report what was found, with timings.
selfcheck: release
	$(SERVER) --self-check tests/fixtures/library --verbose

bench: bench-ferrisjdt

bench-ferrisjdt: release
	$(BENCH) --project tests/fixtures/library \
	  --target src/library/Library.java \
	  --symbol member --receiver "book." --insert-line 30 --runs 50 \
	  --label "FerrisJDT / library" --command $(SERVER)
	@test -d $(CORPUS) && $(BENCH) --project $(CORPUS) \
	  --target org.eclipse.jdt.ls.core/src/org/eclipse/jdt/ls/core/internal/JavaLanguageServerPlugin.java \
	  --symbol logHandler --receiver "logHandler." --insert-line 161 --runs 50 \
	  --label "FerrisJDT / eclipse.jdt.ls" --command $(SERVER) || true

# Measures JDTLS through the same harness, for comparison. Needs `jdtls` on
# PATH and a JDK new enough to run it.
bench-jdtls: release
	$(BENCH) --project tests/fixtures/library \
	  --target src/library/Library.java \
	  --symbol member --receiver "book." --insert-line 30 --runs 50 \
	  --ready-timeout 300 --label "JDTLS / library" --command jdtls

clean:
	cargo clean

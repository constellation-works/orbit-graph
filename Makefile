.DEFAULT_GOAL := help
.PHONY: help build release run dev check test fmt fmt-check clippy doc tree ci ci-fast ci-lint install uninstall clean watch plugin-bundle plugin-check

CARGO ?= cargo
BINARY := orbit-graph
BINARY_PACKAGE := orbit-graph-cli
PROFILE ?= debug
INSTALL_PROFILE ?= release
INSTALL_BIN_DIR ?= $(HOME)/.cargo/bin
ORBIT ?= orbit
CARGO_TARGET_DIR ?= target
export CARGO_TARGET_DIR

ifeq ($(PROFILE),debug)
CARGO_PROFILE :=
else
CARGO_PROFILE := --profile $(PROFILE)
endif

ifeq ($(INSTALL_PROFILE),debug)
INSTALL_CARGO_PROFILE :=
else
INSTALL_CARGO_PROFILE := --profile $(INSTALL_PROFILE)
endif

help:
	@echo "Orbit Graph Make Targets"
	@echo ""
	@echo "  make build        Build (PROFILE=release optional)"
	@echo "  make release      Build optimized binary; does not publish"
	@echo "  make run ARGS=... Run CLI through Cargo"
	@echo "  make dev ARGS=... Build and run binary directly"
	@echo "  make check        Type-check workspace"
	@echo "  make test         Run all tests"
	@echo "  make fmt          Format code"
	@echo "  make fmt-check    Check formatting"
	@echo "  make clippy       Lint all targets (deny warnings)"
	@echo "  make doc          Build documentation (deny warnings)"
	@echo "  make tree         Print dependency feature tree"
	@echo "  make ci           Run complete CONTRIBUTING.md validation"
	@echo "  make ci-fast      Check formatting and diff whitespace"
	@echo "  make ci-lint      Run clippy gate"
	@echo "  make install      Install binary (INSTALL_PROFILE=debug optional)"
	@echo "  make uninstall    Remove binary from INSTALL_BIN_DIR"
	@echo "  make clean        Clean build artifacts"
	@echo "  make watch        Continuous check + test (requires cargo-watch)"
	@echo "  make plugin-bundle Build release binary and bundle it as bin/orbit-graph.bin"
	@echo "  make plugin-check Validate and run the plugin goldens with a fresh build"

build:
	$(CARGO) build --workspace --locked $(CARGO_PROFILE) --target-dir "$(CARGO_TARGET_DIR)"

release:
	$(CARGO) build -p $(BINARY_PACKAGE) --bin $(BINARY) --locked --release --target-dir "$(CARGO_TARGET_DIR)"

run:
	$(CARGO) run -p $(BINARY_PACKAGE) --bin $(BINARY) --locked $(CARGO_PROFILE) --target-dir "$(CARGO_TARGET_DIR)" -- $(ARGS)

dev: build
	"$(CARGO_TARGET_DIR)/$(PROFILE)/$(BINARY)" $(ARGS)

check:
	$(CARGO) check --workspace --locked

test:
	$(CARGO) test --workspace --locked

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked

tree:
	$(CARGO) tree --locked -e features

# Keep the full gate sequential, including when invoked with make -j.
ci:
	$(MAKE) fmt-check
	$(MAKE) clippy
	$(MAKE) test
	$(MAKE) doc
	$(MAKE) build
	git diff --check

ci-fast: fmt-check
	git diff --check

ci-lint: clippy

install:
	$(CARGO) build -p $(BINARY_PACKAGE) --bin $(BINARY) --locked $(INSTALL_CARGO_PROFILE) --target-dir "$(CARGO_TARGET_DIR)"
	install -d "$(INSTALL_BIN_DIR)"
	install -m 755 "$(CARGO_TARGET_DIR)/$(INSTALL_PROFILE)/$(BINARY)" "$(INSTALL_BIN_DIR)/$(BINARY)"

uninstall:
	rm -f "$(INSTALL_BIN_DIR)/$(BINARY)"

clean:
	$(CARGO) clean --target-dir "$(CARGO_TARGET_DIR)"

watch:
	$(CARGO) watch -x "check --workspace --locked" -x "test --workspace --locked"

# Bundle a freshly built release executable beside the plugin launcher
# (bin/orbit-graph.bin, git-ignored), where it wins over PATH.
plugin-bundle: release
	scripts/bundle-plugin-binary.sh --binary "$(CARGO_TARGET_DIR)/release/$(BINARY)" .

# Validate the manifest and run its conformance goldens as a verified
# first-party checkout. A freshly built debug executable is put first on PATH;
# a bundled bin/orbit-graph.bin, when present, still takes precedence.
plugin-check:
	$(CARGO) build -p $(BINARY_PACKAGE) --bin $(BINARY) --locked --target-dir "$(CARGO_TARGET_DIR)"
	$(ORBIT) plugin validate --first-party .
	PATH="$(abspath $(CARGO_TARGET_DIR))/debug:$$PATH" $(ORBIT) plugin test --first-party .

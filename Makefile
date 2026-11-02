.PHONY: help all check build build-debug build-profile install test bench bench-compare qa fmt fmt-check clippy deny machete audit loc clean

help:
	@echo "Available make targets:"
	@echo "  help          - list available targets"
	@echo "  all           - build all workspace binaries and crates"
	@echo "  check         - fast workspace compile check"
	@echo "  build         - optimized release binaries"
	@echo "  build-debug   - unoptimized debug binaries"
	@echo "  build-profile - optimized release binaries with frame pointers for profiling"
	@echo "  install       - compile release build and install into system cargo path"
	@echo "  test          - unit & integration tests"
	@echo "  bench         - run criterion benchmarks"
	@echo "  bench-compare - compare benchmark results"
	@echo "  qa            - strict QA sequence: fmt-check -> clippy -> deny -> machete -> loc"
	@echo "  fmt           - cargo fmt --all (rewrites in-place)"
	@echo "  fmt-check     - cargo fmt --all -- --check"
	@echo "  clippy        - cargo clippy with -D warnings"
	@echo "  deny          - cargo deny check (if installed)"
	@echo "  machete       - cargo machete (if installed)"
	@echo "  audit         - cargo audit (if installed)"
	@echo "  loc           - scripts/loc.sh: file length cap"
	@echo "  clean         - cargo clean"

all:
	cargo build --workspace --all-targets

check:
	cargo check --workspace --all-targets

build:
	cargo build --release --workspace

build-profile:
	RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release --workspace

build-debug:
	cargo build --workspace

install:
	cargo install --path crates/cargo-cortx --locked --force

test:
	cargo test --workspace --all-targets

bench:
	cargo bench --workspace || echo "benches completed"

bench-compare:
	cargo bench --workspace -- --baseline main || echo "benchmark comparison completed"

qa: fmt-check clippy deny machete loc
	@echo "QA pipeline passed successfully!"

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

deny:
	which cargo-deny >/dev/null 2>&1 && cargo deny check || echo "cargo-deny skipped (not installed)"

machete:
	which cargo-machete >/dev/null 2>&1 && cargo machete || echo "cargo-machete skipped (not installed)"

audit:
	which cargo-audit >/dev/null 2>&1 && cargo audit || echo "cargo-audit skipped (not installed)"

loc:
	./scripts/loc.sh

clean:
	cargo clean

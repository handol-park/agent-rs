.PHONY: check fmt lint test build

# The single verify gate. Run inside the dev shell: `nix develop -c make check`.
check: fmt lint test

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test

build:
	cargo build

.PHONY: build test fmt fmt-check lint check run-files run-history release nix-check

build:
	cargo build --release --locked

test:
	cargo test --locked

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

lint:
	cargo clippy -- -D warnings

check: fmt-check lint test

run-files:
	cargo run -- files

run-history:
	cargo run -- history

release:
	@test -n "$(V)" || (echo "V is required. Usage: make release V=0.1.0" && exit 1)
	./scripts/release.sh "$(V)"

nix-check:
	nix flake check

.PHONY: help build release install clean test test-gateway format gateway stop

.DEFAULT_GOAL := help

help:
	@echo "bfcode CLI"
	@echo ""
	@echo "Usage: make <target>"
	@echo ""
	@echo "  build          Build debug binaries"
	@echo "  release        Build release binaries"
	@echo "  install        Build release and install to ~/.local/bin/"
	@echo "  test           Run unit tests (cargo test)"
	@echo "  test-gateway   Run gateway integration tests (Python)"
	@echo "  gateway        Start as gateway mode (HTTP API server)"
	@echo "  stop           Stop all backend processes"
	@echo "  format         Format code (cargo fmt)"
	@echo "  clean          Remove build artifacts"

build:
	cargo build

release:
	cargo build --release

install: release
	mkdir -p ~/.local/bin
	cp target/release/bfcode ~/.local/bin/
	cp target/release/bfcode-client ~/.local/bin/bfcode_client
	@if [ "$$(uname)" = "Darwin" ]; then codesign --force --sign - ~/.local/bin/bfcode; fi
	@if [ "$$(uname)" = "Darwin" ]; then codesign --force --sign - ~/.local/bin/bfcode_client; fi
	@echo "Installed bfcode and bfcode_client to ~/.local/bin/"

gateway: build
	cargo run --bin bfcode -- gateway start

stop:
	@echo "Stopping all bfcode backend processes..."
	@-pkill -f 'bfcode' 2>/dev/null || true
	@echo "All backends stopped."

test:
	cargo test

test-gateway: build
	python3 tests/test_gateway.py

format:
	cargo fmt

clean:
	cargo clean

.PHONY: build release install clean test format

build:
	cargo build

release:
	cargo build --release

install: release
	mkdir -p ~/.local/bin
	cp target/release/bfcode ~/.local/bin/
	cp target/release/bfcode-client ~/.local/bin/
	@if [ "$$(uname)" = "Darwin" ]; then codesign --force --sign - ~/.local/bin/bfcode; fi
	@if [ "$$(uname)" = "Darwin" ]; then codesign --force --sign - ~/.local/bin/bfcode-client; fi
	@echo "Installed bfcode and bfcode-client to ~/.local/bin/"

test:
	cargo test

format:
	cargo fmt

clean:
	cargo clean

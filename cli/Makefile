.PHONY: build release install clean test format

build:
	cargo build

release:
	cargo build --release

install: release
	mkdir -p ~/.local/bin
	cp target/release/bfcode ~/.local/bin/
	@echo "Installed bfcode to ~/.local/bin/bfcode"

test:
	cargo test

format:
	cargo fmt

clean:
	cargo clean

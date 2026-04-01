.PHONY: build test integration install clean

build:
	cargo build --release

test:
	cargo test

integration: build
	bash tests/test_integration.sh

install: build
	cp target/release/memfs /usr/local/bin/
	@echo "Add 'source $(shell pwd)/memfs-init.sh' to your shell profile"

clean:
	cargo clean
	rm -f /tmp/memfs_test*

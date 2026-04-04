.PHONY: build test test-fast test-full integration lint clean

build:
	PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo build --release

test: test-fast

test-fast:
	cargo test --no-default-features --bin memfs

test-full:
	PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo test

integration:
	@if [ ! -f target/release/memfs ]; then $(MAKE) build; fi
	bash tests/test_integration.sh

lint:
	cargo check --no-default-features
	PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo check

clean:
	cargo clean
	rm -f /tmp/memfs_test*

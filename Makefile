.PHONY: build test test-fast test-full integration integration-fuse lint clean

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

# FUSE-specific integration tests. These skip gracefully on machines
# without fuse3/macFUSE, so they're safe to run in CI with best-effort
# coverage. Use this after any change to src/fuse.rs or the init mount
# path in main.rs.
integration-fuse:
	@if [ ! -f target/release/memfs ]; then $(MAKE) build; fi
	bash tests/test_fuse_write.sh

lint:
	cargo check --no-default-features
	PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo check

clean:
	cargo clean
	rm -f /tmp/memfs_test*

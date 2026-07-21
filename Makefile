BINDIR = $(HOME)/.bin
DEBUG_BIN   = target/debug/sst
RELEASE_BIN = target/release/sst

.PHONY: all debug release test install install-debug clean

all: debug

debug:
	cargo build

release:
	cargo build --release

test:
	cargo test

install: release
	install -m 755 $(RELEASE_BIN) $(BINDIR)/sst

install-debug: debug
	install -m 755 $(DEBUG_BIN) $(BINDIR)/sst

clean:
	cargo clean

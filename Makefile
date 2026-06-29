BINDIR = /home/sdk/.bin
TARGET = target/debug/sst

.PHONY: all build install clean

all: build

build:
	cargo build

install: build
	install -m 755 $(TARGET) $(BINDIR)/sst

clean:
	cargo clean

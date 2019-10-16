SHELL = /bin/sh
.SUFFIXES:
.PHONY: all release debug check clean distclean setup migrate
CARGO_JOBS =

ifeq ($(OS),Windows_NT)
	SHELL = cmd
endif

all: release check

release:
ifeq ($(CARGO_JOBS),)
	cargo build --release
else
	cargo build --release -j$(CARGO_JOBS)
endif

debug:
ifeq ($(CARGO_JOBS),)
	cargo build
else
	cargo build -j$(CARGO_JOBS)
endif

check:
ifeq ($(CARGO_JOBS),)
	cargo clippy
else
	cargo clippy -j$(CARGO_JOBS)
endif

clean:
	cargo clean

distclean: clean

setup:
	-cargo check

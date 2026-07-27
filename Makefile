CARGO ?= cargo
INSTALL ?= install
PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DESTDIR ?=

.PHONY: build install

build:
	$(CARGO) build --release --locked --package bspwm-rs --bins

install:
	$(INSTALL) -Dm755 target/release/bspwm-rs $(DESTDIR)$(BINDIR)/bspwm-rs
	$(INSTALL) -Dm755 target/release/bspc-rs $(DESTDIR)$(BINDIR)/bspc-rs

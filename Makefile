.PHONY: help test lint format build build-bundle install clean

PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc
SHAREDIR ?= $(PREFIX)/share
DATADIR ?= $(SHAREDIR)/aish
DOCDIR ?= $(SHAREDIR)/doc/aish
SYSTEMD_UNITDIR ?= /lib/systemd/system
DESTDIR ?=

TARGET ?= x86_64-unknown-linux-musl
NO_BUILD ?= 0

help:
	@echo "AI Shell - Make Commands"
	@echo ""
	@echo "Building:"
	@echo "  make build          Build release binary (musl)"
	@echo "  make build-bundle   Build release bundle archive"
	@echo "  make install        Install built artifacts into DESTDIR/PREFIX"
	@echo ""
	@echo "Development:"
	@echo "  make test           Run tests"
	@echo "  make lint           Run clippy"
	@echo "  make format         Format code"
	@echo "  make format-check   Check code formatting"
	@echo "  make clean          Clean build artifacts"

test:
	cargo test --workspace

lint:
	cargo clippy --all-targets -- -D warnings

format:
	cargo fmt --all

format-check:
	cargo fmt --all -- --check

build:
	./build.sh

build-bundle:
	./packaging/build_bundle.sh

install:
	@if [ "$(NO_BUILD)" != "1" ]; then \
		$(MAKE) build; \
	fi
	@echo "Installing built artifacts into $(DESTDIR)"
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 target/$(TARGET)/release/aish "$(DESTDIR)$(BINDIR)/aish"
	install -d "$(DESTDIR)$(SYSCONFDIR)/aish"
	install -m 0644 config/security_policy.yaml "$(DESTDIR)$(SYSCONFDIR)/aish/security_policy.yaml"
	install -d "$(DESTDIR)$(SYSTEMD_UNITDIR)"
	install -m 0644 debian/aish-sandbox.service "$(DESTDIR)$(SYSTEMD_UNITDIR)/aish-sandbox.service"
	install -m 0644 debian/aish-sandbox.socket "$(DESTDIR)$(SYSTEMD_UNITDIR)/aish-sandbox.socket"
	install -d "$(DESTDIR)$(DOCDIR)"
	install -m 0644 docs/skills-guide.md "$(DESTDIR)$(DOCDIR)/skills-guide.md"
	@if [ -d debian/skills ]; then \
		install -d "$(DESTDIR)$(DATADIR)"; \
		cp -a debian/skills "$(DESTDIR)$(DATADIR)/"; \
	fi

clean:
	cargo clean
	rm -rf dist/ build/ artifacts/

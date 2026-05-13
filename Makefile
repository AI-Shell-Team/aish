.PHONY: help test packaging-test prepare-release-files lint format build build-binary build-bundle install clean

PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc
SHAREDIR ?= $(PREFIX)/share
DATADIR ?= $(SHAREDIR)/aish
DOCDIR ?= $(SHAREDIR)/doc/aish
SYSTEMD_UNITDIR ?= /etc/systemd/system
DESTDIR ?=

TARGET ?= x86_64-unknown-linux-musl
NO_BUILD ?= 0
PYTHON ?= python3

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
	@echo "  make packaging-test Run release script smoke tests"
	@echo "  make prepare-release-files VERSION=X.Y.Z[-PRERELEASE] [DATE=YYYY-MM-DD]"
	@echo "  make lint           Run clippy"
	@echo "  make format         Format code"
	@echo "  make format-check   Check code formatting"
	@echo "  make clean          Clean build artifacts"

test:
	cargo test --workspace
	PYTHON="$(PYTHON)" ./packaging/tests/release_scripts_smoke.sh

packaging-test:
	PYTHON="$(PYTHON)" ./packaging/tests/release_scripts_smoke.sh

prepare-release-files:
	@test -n "$(VERSION)" || { echo "VERSION is required, for example: make prepare-release-files VERSION=1.0.0-beta.1" >&2; exit 2; }
	$(PYTHON) packaging/scripts/update_release_files.py --version "$(VERSION)" $(if $(DATE),--date "$(DATE)",)

lint:
	cargo clippy --all-targets -- -D warnings

format:
	cargo fmt --all

format-check:
	cargo fmt --all -- --check

build:
	./build.sh

build-binary: build

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
	install -d "$(DESTDIR)$(DOCDIR)"
	install -m 0644 docs/skills-guide.md "$(DESTDIR)$(DOCDIR)/skills-guide.md"
	install -d "$(DESTDIR)$(SYSTEMD_UNITDIR)"
	sed 's|@AISH_BINDIR@|$(BINDIR)|g' packaging/systemd/aish-sandbox.service.in > "$(DESTDIR)$(SYSTEMD_UNITDIR)/aish-sandbox.service"
	chmod 0644 "$(DESTDIR)$(SYSTEMD_UNITDIR)/aish-sandbox.service"
	install -m 0644 packaging/systemd/aish-sandbox.socket "$(DESTDIR)$(SYSTEMD_UNITDIR)/aish-sandbox.socket"
	@if [ -d debian/skills ]; then \
		install -d "$(DESTDIR)$(DATADIR)"; \
		cp -a debian/skills "$(DESTDIR)$(DATADIR)/"; \
	fi

clean:
	cargo clean
	rm -rf dist/ build/ artifacts/

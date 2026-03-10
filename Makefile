.PHONY: help deps dev test lint format build build-binary install clean nfpm nfpm-deb nfpm-rpm nfpm-apk release

PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc
SHAREDIR ?= $(PREFIX)/share
DATADIR ?= $(SHAREDIR)/aish
DOCDIR ?= $(SHAREDIR)/doc/aish
SYSTEMD_UNITDIR ?= /lib/systemd/system
DESTDIR ?=

NO_BUILD ?= 0

# Default target
help:
	@echo "🚀  AI Shell - Make Commands"
	@echo ""
	@echo "Dependencies:"
	@echo "  make deps           Install project dependencies"
	@echo "  make dev            Install dev dependencies"
	@echo "  make test           Run tests"
	@echo "  make lint           Run linting"
	@echo "  make format         Format code"
	@echo ""
	@echo "Building:"
	@echo "  make build         Build Python wheel"
	@echo "  make build-binary  Build standalone binaries"
	@echo "  make install       Install built artifacts into DESTDIR/PREFIX"
	@echo "  make clean          Clean build artifacts"
	@echo ""
	@echo "Release Packaging (nfpm):"
	@echo "  make nfpm          Build all package formats (deb, rpm, apk)"
	@echo "  make nfpm-deb      Build Debian/Ubuntu package"
	@echo "  make nfpm-rpm      Build RHEL/Fedora/CentOS package"
	@echo "  make nfpm-apk      Build Alpine package"
	@echo "  make release       Full release: build binaries + all packages"

deps:
	@echo "📦 Installing dependencies..."
	uv sync

dev:
	@echo "📦 Installing dev dependencies..."
	uv sync --group dev

test:
	@echo "🧪 Running tests..."
	uv run --group dev pytest tests/ -v

lint:
	@echo "🔍 Running linting..."
	uv run --group dev ruff check src/ tests/
	uv run --group dev mypy src/

format:
	@echo "🎨 Formatting code..."
	uv run --group dev ruff format src/ tests/
	uv run --group dev ruff check --fix src/ tests/

build:
	@echo "📦 Building Python wheel..."
	uv build

build-binary:
	@echo "🔨 Building standalone binaries..."
	./build.sh

install:
	@if [ "$(NO_BUILD)" != "1" ]; then \
		$(MAKE) build-binary; \
	fi
	@echo "📥 Installing built artifacts into $(DESTDIR)"
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 dist/aish "$(DESTDIR)$(BINDIR)/aish"
	install -m 0755 dist/aish-sandbox "$(DESTDIR)$(BINDIR)/aish-sandbox"
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
	@echo "🧹 Cleaning build artifacts..."
	rm -rf dist/ build/ .build-venv/ *.spec.backup __pycache__/ .pytest_cache/
	find . -name "*.pyc" -delete
	find . -name "*.pyo" -delete
	find . -name "*.egg-info" -exec rm -rf {} +

# NFPM release packaging targets
nfpm: nfpm-deb nfpm-rpm nfpm-apk
	@echo "✅ All nfpm packages built successfully!"

nfpm-deb: build-binary
	@echo "📦 Building .deb package with nfpm..."
	@if ! command -v nfpm >/dev/null 2>&1; then \
		echo "❌ nfpm not found. Install it first:"; \
		echo "   pip install nfpm"; \
		echo "   Or download from: https://github.com/goreleaser/nfpm/releases"; \
		exit 1; \
	fi
	mkdir -p releases
	nfpm pkg -f nfpm.yaml -p deb -t releases/
	@echo "✅ .deb package built: releases/aish_*.deb"

nfpm-rpm: build-binary
	@echo "📦 Building .rpm package with nfpm..."
	@if ! command -v nfpm >/dev/null 2>&1; then \
		echo "❌ nfpm not found. Install it first:"; \
		echo "   pip install nfpm"; \
		echo "   Or download from: https://github.com/goreleaser/nfpm/releases"; \
		exit 1; \
	fi
	mkdir -p releases
	nfpm pkg -f nfpm.yaml -p rpm -t releases/
	@echo "✅ .rpm package built: releases/aish-*.rpm"

nfpm-apk: build-binary
	@echo "📦 Building .apk package with nfpm..."
	@if ! command -v nfpm >/dev/null 2>&1; then \
		echo "❌ nfpm not found. Install it first:"; \
		echo "   pip install nfpm"; \
		echo "   Or download from: https://github.com/goreleaser/nfpm/releases"; \
		exit 1; \
	fi
	mkdir -p releases
	nfpm pkg -f nfpm.yaml -p apk -t releases/
	@echo "✅ .apk package built: releases/aish_*.apk"

release: clean build-binary nfpm
	@echo ""
	@echo "🎉 Release build completed!"
	@echo "📦 Packages available in releases/ directory:"
	@ls -lh releases/ || true
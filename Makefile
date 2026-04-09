.PHONY: help deps dev test test-live-smoke lint format build build-binary build-bundle prepare-release-files install clean

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
	@echo "  make test-live-smoke Run opt-in live smoke tests"
	@echo "  make lint           Run linting"
	@echo "  make format         Format code"
	@echo ""
	@echo "Building:"
	@echo "  make build          Build Python wheel"
	@echo "  make build-binary   Build standalone binaries"
	@echo "  make build-bundle   Build release bundle archive"
	@echo "  make prepare-release-files VERSION=X.Y.Z [DATE=YYYY-MM-DD]  Update release versions and add a changelog section"
	@echo "  make install        Install built artifacts into DESTDIR/PREFIX"
	@echo "  make clean          Clean build artifacts"

deps:
	@echo "📦 Installing dependencies..."
	uv sync

dev:
	@echo "📦 Installing dev dependencies..."
	uv sync --group dev

test:
	@echo "🧪 Running tests..."
	uv run --group dev python -m pytest tests/ -v

test-live-smoke:
	@echo "🧪 Running live smoke tests..."
	uv run --group dev python -m pytest tests/flows/live_smoke/ -v -m live_smoke --run-live-smoke

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

build-bundle:
	@echo "📦 Building release bundle..."
	./packaging/build_bundle.sh

prepare-release-files:
	@if [ -z "$(VERSION)" ]; then \
		echo "Usage: make prepare-release-files VERSION=X.Y.Z [DATE=YYYY-MM-DD]"; \
		exit 1; \
	fi
	@echo "📝 Updating release files for $(VERSION)..."
	@python3 packaging/scripts/update_release_files.py --version "$(VERSION)" $(if $(DATE),--date "$(DATE)",)

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
	@if [ -d src/aish/scripts/themes ]; then \
		install -d "$(DESTDIR)$(DATADIR)/scripts/themes"; \
		install -m 0644 src/aish/scripts/themes/*.aish "$(DESTDIR)$(DATADIR)/scripts/themes/"; \
		install -m 0644 src/aish/scripts/themes/THEMES.md "$(DESTDIR)$(DATADIR)/scripts/themes/"; \
	fi
	@if [ -d src/aish/scripts/templates ]; then \
		install -d "$(DESTDIR)$(DATADIR)/scripts/templates"; \
		install -m 0644 src/aish/scripts/templates/*.aish "$(DESTDIR)$(DATADIR)/scripts/templates/"; \
	fi

clean:
	@echo "🧹 Cleaning build artifacts..."
	rm -rf dist/ build/ .build-venv/ *.spec.backup __pycache__/ .pytest_cache/
	find . -name "*.pyc" -delete
	find . -name "*.pyo" -delete
	find . -name "*.egg-info" -exec rm -rf {} +
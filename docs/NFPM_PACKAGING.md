# NFPM Release Packaging Guide

This document describes how to use [nfpm](https://nfpm.goreleaser.com/) to build release packages for AI Shell.

## Overview

[nfpm](https://nfpm.goreleaser.com/) is a simple, lightweight, and fast packager for creating `.deb`, `.rpm`, and `.apk` packages. It's designed as a zero-dependency alternative to traditional packaging tools.

## Prerequisites

### Install nfpm

You can install nfpm using one of these methods:

**Method 1: Using pip (recommended)**
```bash
pip install nfpm
```

**Method 2: Download binary**
Download the latest release from [nfpm GitHub releases](https://github.com/goreleaser/nfpm/releases)

**Method 3: Using Homebrew (macOS)**
```bash
brew install nfpm
```

### Build Binaries First

Before creating packages, you need to build the binaries:

```bash
make build-binary
# or
./build.sh
```

## Quick Start

### Build All Package Formats

```bash
# Using Makefile
make nfpm

# Or using the helper script
./packaging/build_with_nfpm.sh all
```

### Build Specific Format

**Debian/Ubuntu (.deb):**
```bash
make nfpm-deb
# or
./packaging/build_with_nfpm.sh deb
```

**RHEL/Fedora/CentOS (.rpm):**
```bash
make nfpm-rpm
# or
./packaging/build_with_nfpm.sh rpm
```

**Alpine (.apk):**
```bash
make nfpm-apk
# or
./packaging/build_with_nfpm.sh apk
```

## Custom Version and Release

By default, the version is extracted from `pyproject.toml`. You can override it:

```bash
# Using environment variables
VERSION=1.2.3 RELEASE=2 make nfpm-deb

# Or using the helper script
./packaging/build_with_nfpm.sh-v 1.2.3 -r 2 deb

# Full release build
VERSION=1.2.3 make release
```

## Configuration

The main configuration file is [`nfpm.yaml`](../nfpm.yaml). Key sections:

- **name**: Package name (`aish`)
- **version**: Package version (from env or git)
- **arch**: Target architecture (amd64, arm64, etc.)
- **contents**: Files to include in the package
- **overrides**: Format-specific customizations

### Package Contents

The following files are included:

- `/usr/bin/aish` - Main CLI binary
- `/usr/bin/aish-sandbox` - Sandbox daemon binary
- `/etc/aish/security_policy.yaml` - Security configuration
- `/lib/systemd/system/aish-sandbox.service` - Systemd service
- `/lib/systemd/system/aish-sandbox.socket` - Systemd socket
- `/usr/share/doc/aish/skills-guide.md` - Documentation
- `/usr/share/aish/skills/` - Skills directory

## Output

Packages are generated in the `releases/` directory:

```
releases/
├── aish_1.2.3_amd64.deb
├── aish-1.2.3-1.x86_64.rpm
└── aish_1.2.3_amd64.apk
```

## Installation

### Install .deb Package (Debian/Ubuntu)

```bash
sudo dpkg -i releases/aish_*.deb
# or
sudo apt install ./releases/aish_*.deb
```

### Install .rpm Package (RHEL/Fedora/CentOS)

```bash
sudo rpm -ivh releases/aish-*.rpm
# or
sudo dnf install ./releases/aish-*.rpm
```

### Install .apk Package (Alpine)

```bash
sudo apk add --allow-untrusted releases/aish_*.apk
```

## Post-Installation

After installation, the package will:

1. Create necessary directories (`/etc/aish`, `/usr/share/aish`)
2. Install systemd service files
3. Enable the sandbox service (if systemd is available)
4. Set up default security policies

### Verify Installation

```bash
# Check binary location
which aish
which aish-sandbox

# Check version
aish --version

# Check systemd service
systemctl status aish-sandbox.service
```

## Advanced Usage

### Cross-Compilation

To build for different architectures:

```bash
# Build for ARM64
ARCH=aarch64 VERSION=1.2.3 make nfpm-deb

# Using helper script
./packaging/build_with_nfpm.sh-a aarch64 deb
```

### Custom Scripts

Post-install and post-remove scripts are located in:
- `debian/aish.postinst` - Runs after installation
- `debian/aish.postrm` - Runs after removal

### Debugging

Enable verbose output:

```bash
NFPM_DEBUG=1 make nfpm-deb
```

## Comparison with Traditional Method

| Feature | nfpm | dpkg-buildpackage |
|---------|------|-------------------|
| Speed | Fast | Slower |
| Dependencies | None | Many (debhelper, etc.) |
| Cross-platform | Yes | Linux only |
| Configuration | Simple YAML | Complex debian/* |
| Learning Curve | Low | High |

## Troubleshooting

### Common Issues

**Issue: nfpm not found**
```bash
# Install nfpm
pip install nfpm
```

**Issue: Binaries not found**
```bash
# Build binaries first
make build-binary
```

**Issue: Permission denied**
```bash
# Ensure binaries are executable
chmod +x dist/aish dist/aish-sandbox
```

**Issue: Missing dependencies**
```bash
# For .deb: Install required packages
sudo apt install bubblewrap util-linux

# For .rpm: Install required packages
sudo dnf install bubblewrap util-linux
```

## Migration from dpkg-buildpackage

If you're currently using the traditional Debian packaging method:

1. **Keep existing debian/ directory** for backward compatibility
2. **Use nfpm for new releases** - faster and simpler
3. **Test both methods** during transition period

Example workflow:
```bash
# Old way (still works)
packaging/build_deb.sh

# New way (recommended)
make nfpm-deb
```

## References

- [nfpm Documentation](https://nfpm.goreleaser.com/)
- [nfpm GitHub Repository](https://github.com/goreleaser/nfpm)
- [DEB Package Format](https://www.debian.org/doc/debian-policy/)
- [RPM Package Format](https://rpm.org/documentation.html)

## License

MIT License - See [LICENSE](../LICENSE) for details.

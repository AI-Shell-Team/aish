# NFPM Quick Start Guide

## 快速使用 nfpm 打包

### 1. 安装 nfpm

```bash
pip install nfpm
```

或者从 [GitHub Releases](https://github.com/goreleaser/nfpm/releases) 下载二进制文件。

### 2. 构建二进制文件

```bash
make build-binary
```

### 3. 生成安装包

**构建 .deb 包（Debian/Ubuntu）：**
```bash
make nfpm-deb
```

**构建 .rpm 包（RHEL/Fedora/CentOS）：**
```bash
make nfpm-rpm
```

**构建所有格式：**
```bash
make nfpm
```

### 4. 安装包

```bash
# Debian/Ubuntu
sudo dpkg -i releases/aish_*.deb

# RHEL/Fedora/CentOS
sudo rpm -ivh releases/aish-*.rpm
```

## 自定义版本

```bash
VERSION=1.2.3 RELEASE=2 make nfpm-deb
```

## 输出位置

生成的包位于 `releases/` 目录。

---

详细文档请查看：[docs/NFPM_PACKAGING.md](docs/NFPM_PACKAGING.md)

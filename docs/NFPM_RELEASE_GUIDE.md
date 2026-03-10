# AI Shell Release Packaging with nfpm

## 概述 (Overview)

本项目使用 [nfpm](https://nfpm.goreleaser.com/) 来构建 release 包。nfpm 是一个简单、轻量且快速的打包工具，可以生成 `.deb`、`.rpm` 和 `.apk` 格式的 Linux 安装包。

## 为什么使用 nfpm？

- ✅ **零依赖** - 不需要安装 debhelper、rpmbuild 等复杂工具
- ✅ **跨平台** - 可以在任何支持 nfpm 的系统上打包
- ✅ **配置简单** - 使用 YAML 格式，易于理解和维护
- ✅ **快速构建** - 比传统打包工具快数倍
- ✅ **一致性** - 同一配置可生成多种包格式

## 快速开始

### 1. 安装 nfpm

```bash
# 方法 1: 使用 pip（推荐）
pip install nfpm

# 方法 2: 从 GitHub Releases 下载
# https://github.com/goreleaser/nfpm/releases

# 方法 3: macOS 使用 Homebrew
brew install nfpm
```

### 2. 构建二进制文件

在打包之前，需要先构建二进制文件：

```bash
make build-binary
# 或者
./build.sh
```

### 3. 生成安装包

#### 构建所有格式

```bash
# 使用 Makefile
make nfpm

# 或使用辅助脚本
./packaging/build_with_nfpm.sh all
```

#### 构建特定格式

```bash
# Debian/Ubuntu (.deb)
make nfpm-deb
./packaging/build_with_nfpm.sh deb

# RHEL/Fedora/CentOS (.rpm)
make nfpm-rpm
./packaging/build_with_nfpm.sh rpm

# Alpine Linux (.apk)
make nfpm-apk
./packaging/build_with_nfpm.sh apk
```

### 4. 自定义版本

```bash
# 方式 1: 使用环境变量
VERSION=1.2.3 RELEASE=2 make nfpm-deb

# 方式 2: 使用辅助脚本的参数
./packaging/build_with_nfpm.sh -v 1.2.3 -r 2 deb
```

## 配置文件说明

### nfpm.yaml

主配置文件 [`nfpm.yaml`](../nfpm.yaml) 包含以下关键部分：

```yaml
name: aish                    # 包名
arch: amd64                   # 目标架构
platform: linux               # 目标平台
version: ${VERSION}           # 版本号（可从环境变量读取）
release: ${RELEASE:-1}        # 发布号
maintainer: ...               # 维护者信息
description: |                # 包描述
  AI Shell 功能说明...

contents:                     # 包含的文件
  - src: dist/aish
    dst: /usr/bin/aish
    mode: 0755

overrides:                    # 不同包格式的特定配置
  deb:
    dependencies:
      - bubblewrap
      - util-linux
```

### 包含的文件

打包后会在系统中安装以下文件：

| 文件 | 目标路径 | 说明 |
|------|---------|------|
| `dist/aish` | `/usr/bin/aish` | 主 CLI 程序 |
| `dist/aish-sandbox` | `/usr/bin/aish-sandbox` | 沙箱守护进程 |
| `config/security_policy.yaml` | `/etc/aish/security_policy.yaml` | 安全策略配置 |
| `debian/aish-sandbox.service` | `/lib/systemd/system/aish-sandbox.service` | systemd 服务（deb/apk）|
| `debian/aish-sandbox.socket` | `/lib/systemd/system/aish-sandbox.socket` | systemd socket（deb/apk）|
| `debian/aish-sandbox.service` | `/usr/lib/systemd/system/aish-sandbox.service` | systemd 服务（rpm）|
| `debian/aish-sandbox.socket` | `/usr/lib/systemd/system/aish-sandbox.socket` | systemd socket（rpm）|
| `docs/skills-guide.md` | `/usr/share/doc/aish/skills-guide.md` | 文档 |
| `debian/skills/*` | `/usr/share/aish/skills/` | 技能插件 |

## 安装生成的包

### Debian/Ubuntu

```bash
# 使用 dpkg
sudo dpkg -i releases/aish_*.deb

# 使用 apt
sudo apt install ./releases/aish_*.deb

# 验证安装
aish --version
systemctl status aish-sandbox
```

### RHEL/Fedora/CentOS

```bash
# 使用 rpm
sudo rpm -ivh releases/aish-*.rpm

# 使用 dnf
sudo dnf install ./releases/aish-*.rpm

# 验证安装
rpm -qi aish
```

### Alpine Linux

```bash
sudo apk add --allow-untrusted releases/aish_*.apk
```

## 高级用法

### 交叉编译

为不同架构构建包：

```bash
# ARM64
ARCH=aarch64 VERSION=1.2.3 make nfpm-deb

# ARMv7
ARCH=armv7l VERSION=1.2.3 make nfpm-rpm
```

### 完整 Release 流程

```bash
# 清理并重新构建
make clean
make build-binary
make nfpm

# 或一键完成
make release
```

### 调试模式

```bash
# 启用 nfpm 调试输出
NFPM_DEBUG=1 make nfpm-deb

# 查看详细日志
./packaging/build_with_nfpm.sh deb 2>&1 | tee build.log
```

## GitHub Actions 自动构建

项目配置了 GitHub Actions workflow，当推送版本标签时会自动构建：

```bash
# 推送版本标签
git tag v1.2.3
git push origin v1.2.3
```

Actions 会：
1. 检出代码
2. 安装依赖
3. 构建二进制文件
4. 生成所有格式的包
5. 创建 GitHub Release 并上传产物

## 故障排除

### 常见问题

**问题 1: nfpm 未安装**
```bash
pip install nfpm
```

**问题 2: 找不到二进制文件**
```bash
# 先构建二进制
make build-binary
```

**问题 3: 权限错误**
```bash
# 确保二进制文件可执行
chmod +x dist/aish dist/aish-sandbox
```

**问题 4: 依赖缺失**
```bash
# 安装必要的系统依赖
sudo apt install bubblewrap util-linux  # Debian/Ubuntu
sudo dnf install bubblewrap util-linux  # RHEL/Fedora
```

### 验证包内容

```bash
# 查看 .deb 包内容
dpkg -c releases/aish_*.deb

# 查看 .rpm 包内容
rpm -qlp releases/aish-*.rpm

# 查看 .apk 包内容
tar -tzf releases/aish_*.apk
```

## 与传统方法的对比

| 特性 | nfpm | dpkg-buildpackage |
|------|------|-------------------|
| 安装依赖 | 无 | debhelper, dh-* 等 |
| 配置复杂度 | 简单 YAML | 复杂的 debian/* 目录 |
| 构建速度 | 快 | 较慢 |
| 跨平台 | 是 | 仅 Linux |
| 学习曲线 | 低 | 高 |
| 适用场景 | 现代 CI/CD | 传统 Debian 打包 |

## 最佳实践

1. **版本管理**: 使用语义化版本（Semantic Versioning）
2. **自动化**: 使用 GitHub Actions 自动构建
3. **测试**: 在目标系统上测试安装的包
4. **文档**: 保持 CHANGELOG.md 更新
5. **签名**: 生产环境考虑对包进行签名

## 相关资源

- [nfpm 官方文档](https://nfpm.goreleaser.com/)
- [nfpm GitHub](https://github.com/goreleaser/nfpm)
- [DEB 包规范](https://www.debian.org/doc/debian-policy/)
- [RPM 包规范](https://rpm.org/documentation.html)
- [AI Shell 开发文档](../CONTRIBUTING.md)

## 许可证

MIT License - 详见 [LICENSE](../LICENSE) 文件

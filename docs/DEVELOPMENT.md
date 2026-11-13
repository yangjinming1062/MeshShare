# MeshShare 开发指南

## 环境准备

安装 Git、Rust stable 和 Node.js 22，准备对应系统的构建依赖。Windows 需要 MSVC 和 WebView2，macOS 需要 Xcode 命令行工具，Ubuntu 24.04 需要 WebKitGTK 4.1 和 Ayatana AppIndicator 等开发包。[Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/#system-dependencies)

以下命令从仓库根目录执行。Tauri CLI 由 `npx` 按需运行。

```bash
npm --prefix web ci
npm --prefix web run build
```

手机扫码访问的页面由桌面节点从 `web/dist` 构建产物提供。改动移动网页后，重新构建前端并重启桌面程序，再刷新手机页面。

## 更新项目图标

项目标志和网页共用 `web/public/meshshare.svg`。修改源图后，从仓库根目录运行 Tauri CLI 生成桌面图标。

```bash
npx --yes @tauri-apps/cli@2 icon web/public/meshshare.svg --output src-tauri/target/meshshare-icon-generation
```

将生成的 ICO、ICNS 和 `src-tauri/tauri.conf.json` 列出的 PNG 更新到 `src-tauri/icons/`。这些文件随仓库维护，日常构建直接使用。

## 启动与构建

启动桌面开发窗口。Tauri 会自动启动 Vite。

```bash
npx --yes @tauri-apps/cli@2 dev -- --locked
```

构建本地调试程序，产物位于 `src-tauri/target/debug/`。

```bash
npx --yes @tauri-apps/cli@2 build --debug --no-bundle -- --locked
```

推送 `v*` tag 时，[GitHub Actions 发布工作流](../.github/workflows/release.yml) 构建各平台的安装包，再逐个上传到该 tag 的 GitHub Release。普通分支推送和 Pull Request 的检查在本地运行。

## 检查

Rust 格式和 lint 检查需要 `rustfmt` 和 `clippy` 组件，可通过 `rustup component add rustfmt clippy` 安装。

```bash
npm --prefix web run check
npm --prefix web run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

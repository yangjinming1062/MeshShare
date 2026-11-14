# MeshShare 开发指南

## 环境准备

安装 Git、Rust stable（当前 Tauri 2 依赖链和 single-instance 插件要求 Rust 1.90 或更高）和 Node.js 22，准备对应系统的构建依赖。Windows 需要 MSVC 和 WebView2，macOS 需要 Xcode 命令行工具，Ubuntu 24.04 需要 WebKitGTK 4.1 和 Ayatana AppIndicator 等开发包。[Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/#system-dependencies)

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

开发模式和调试构建不会自动写入系统登录启动项，避免把调试可执行文件注册为正式启动入口；调试界面也不显示不可用的自启动开关。开机自启动需要在正式桌面安装包中验证：首次启动应默认注册，登录后主窗口保持隐藏并可从托盘打开，重复打开应用只激活已有窗口。Windows NSIS/MSI 和 Linux deb/rpm 卸载钩子会清理自启动注册。

推送 `vMAJOR.MINOR.PATCH` tag 时，[GitHub Actions 发布工作流](../.github/workflows/release.yml) 将 tag 版本注入 Tauri 构建，用于安装包元数据和节点报告的版本。工作流逐个上传 Windows x64、macOS Intel 和 Apple Silicon、Linux x64 的七个安装包，核对 GitHub Release 的附件完整后自动发布。普通分支推送和 Pull Request 的检查在本地运行。

发布说明由 OpenAI 兼容接口根据上一版本到当前 tag 的提交记录生成。`LLM_API_KEY` 取自 Actions Secrets，`LLM_BASE_URL` 和 `LLM_MODEL_NAME` 可配置为 Actions Secrets 或 Variables。生成失败时发布任务等待重跑。

发布失败后可重跑失败的任务，沿用 tag 和已构建的安装包。如果发布任务报告有额外附件，先到对应 Release 核对并清理旧附件，再重跑发布任务。

## 检查

Rust 格式和 lint 检查需要 `rustfmt` 和 `clippy` 组件，可通过 `rustup component add rustfmt clippy` 安装。

```bash
npm --prefix web run check
npm --prefix web run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

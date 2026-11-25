# MeshShare 开发指南

## 环境准备

使用 Git、Node.js 22 和 Rust stable（`Cargo.toml` 要求 1.90 或更高版本）。桌面构建还需要对应平台的 Tauri 依赖。Windows 使用 MSVC 和 WebView2，macOS 使用 Xcode 命令行工具。Ubuntu 24.04 的依赖由 [CI](../.github/workflows/ci.yml) 安装，包括 WebKitGTK 4.1、Wayland 开发库、librsvg、patchelf 和 Ayatana AppIndicator。[Tauri 系统依赖](https://v2.tauri.app/start/prerequisites/#system-dependencies)

从仓库根目录安装前端依赖。

```sh
npm --prefix frontend ci
```

## 启动与构建

### 桌面

```sh
npm --prefix backend run tauri -- dev -- --locked
npm --prefix backend run tauri -- build --debug --no-bundle -- --locked
```

两条命令分别启动开发环境和生成 `backend/target/debug/` 下的调试程序。调试节点的浏览器页面来自 `frontend/dist`，修改后运行 `npm --prefix frontend run build`，再重启节点并刷新页面。

### Android

准备 Python 3、JDK 21，以及 Android SDK Platform 36、Build Tools 36.0.0 和 NDK 27.2.12479018。以下命令以 arm64 真机为例，先将 `JAVA_HOME` 和 `ANDROID_HOME` 指向本机安装目录。

```sh
export NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
rustup target add aarch64-linux-android
npm --prefix backend run tauri -- android init --ci --skip-targets-install
python3 .github/scripts/prepare_android_project.py
npm --prefix backend run tauri -- android dev
```

构建可在 arm64 真机安装的调试 APK，或为分发生成 AAB。

```sh
npm --prefix backend run tauri -- android build --debug --apk --target aarch64
npm --prefix backend run tauri -- android build --aab --target aarch64
```

发布签名按[发布工作流](../.github/workflows/release.yml)配置。Android 最低要求 Android 10（API 29），App 正式构建以 Chromium 108 为目标。生成的工程在 `backend/gen/android/`，原生插件在 `backend/android/`。改动插件、Android 权限或资源后重跑准备脚本。保留 `usesCleartextTraffic` 以连接局域网节点。

### Linux-headless 版

构建环境使用 Ubuntu 22.04 amd64。打包脚本检查 amd64、glibc 2.35 兼容性和桌面动态库依赖，安装需要 systemd 249 或更高版本。将 `version` 设为包版本后运行以下命令。

```sh
version=0.0.0
npm --prefix frontend run build
MESHSHARE_VERSION="$version" cargo build --manifest-path backend/Cargo.toml --locked --release --no-default-features --features headless --bin meshshare-headless
backend/packaging/headless/build-deb.sh "$version" backend/target/release/meshshare-headless dist/headless
```

## 检查

```sh
rustup component add rustfmt clippy
npm --prefix frontend run check
npm --prefix frontend test
npm --prefix frontend run build
cargo fmt --manifest-path backend/Cargo.toml --all -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --all-features --locked
```

[CI](../.github/workflows/ci.yml) 在推送到 `main` 时运行 Ubuntu 静态检查、桌面各平台的 Rust 测试、Linux-headless 版打包和 systemd 验收，以及 Android 调试 APK 构建。提交 Pull Request 前在本地运行相关检查。

Linux-headless 版单独验证无桌面依赖的构建。

```sh
cargo clippy --manifest-path backend/Cargo.toml --all-targets --no-default-features --features headless --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --no-default-features --features headless --locked
```

系统服务验收使用一次性 Linux 环境。安装前参照 CI 为服务配置只有回环接口的网络命名空间，隔离生产更新源。安装后在同一命名空间以普通用户执行 `meshshare-headless status` 和 `node --test backend/tests/headless-smoke.mjs`。后者会写入剪贴板历史和样例文件。重装后检查运行版本、自启配置和数据是否保留。

## 发布

推送 `vMAJOR.MINOR.PATCH` 标签触发[发布工作流](../.github/workflows/release.yml)。工作流以标签设定产物版本，验证并上传安装包和更新清单，发布为预发行版。完成平台验收后在 GitHub 将它晋级为稳定版，客户端从稳定版渠道获取更新。

| 产物 | 架构与格式 |
| --- | --- |
| Windows 桌面 | x64，EXE（NSIS）、MSI |
| macOS 桌面 | Intel、Apple Silicon，DMG（更新使用 `.app.tar.gz`） |
| Linux 桌面 | x64，AppImage、DEB、RPM |
| Linux-headless 版 | amd64，独立 DEB 和 SHA-256 校验文件 |
| Android | arm64 签名 APK |
| 更新清单 | `latest.json`，包含桌面、Linux-headless 版的签名和 Android 包的摘要 |

- 发布说明由[生成脚本](../.github/scripts/gen_release_notes.py)根据提交生成，并追加[下载指南](../.github/release-download-guide.md)。`LLM_API_KEY` 使用 Actions Secret，`LLM_BASE_URL` 和 `LLM_MODEL_NAME` 可用 Secret 或 Variable。
- 桌面与 Linux-headless 版的更新使用 Tauri 签名密钥。首次配置以 `npm --prefix backend run tauri -- signer generate --write-keys ~/.tauri/meshshare-updater.key` 在仓库外生成密钥，将公钥写入 [Tauri 配置](../backend/tauri.conf.json)，把原样的单行私钥存入 `TAURI_SIGNING_PRIVATE_KEY`，密码存入 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。客户端使用已内置的公钥验证更新，更换公钥需要手动升级已安装的客户端。
- macOS 安装包采用 ad-hoc 签名。Android 覆盖安装须保持发布证书一致，通过 `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASSWORD` 配置签名。版本号与签名校验见[发布工作流](../.github/workflows/release.yml)和[清单生成脚本](../.github/scripts/generate_updater_manifest.py)。
- 重跑失败任务沿用标签对应的源码。修复发布脚本后用指向修复提交的新版本标签。重跑时按工作流报错检查多余附件，已晋级的稳定版受工作流保护。

## 平台验收

用正式安装包、真实设备和目标系统检查受影响的功能，并记录环境和结果。

| 走查内容 | 覆盖点 |
| --- | --- |
| 桌面安装与托盘 | 首次运行、登录自启、单实例、关窗后常驻、托盘恢复与卸载清理。Windows 分别检查单击、双击、右键。 |
| 连接 | 占用首选端口，验证局域网和跨物理网络的 Tailscale 自动发现备用端口。覆盖多网卡、全部端口占用、单向和双向端口可达及换网。保持 Tailscale 节点在线，让 MeshShare 重启到已进入探测退避的备用端口，验证下一轮发现能够重连。 |
| 剪贴板与文件 | 文字、大 PNG、历史恢复、跨平台目录传输、断线重试和同名文件。限速时检查剪贴板最新内容、传输进度和网页会话是否都能正常更新。 |
| 网页与远程管理 | 扫码、网页与宿主互传、从目标磁盘发给网页。在上传或目录读取期间切换管理目标，确认任务仍归正确节点。覆盖同名节点、断线重试、远程设置并发更改。 |
| 终端 | 用桌面安装包、Android 真机连接目标，验证输入法、快捷键、真彩色、横竖屏或缩放。覆盖待确认状态、本机确认授权及其跨重启保留、断线后关闭会话和活动终端延后更新。 |
| 悬浮条与 Wayland | Windows、macOS、X11 检查全屏显隐、穿透、混合 DPI、多屏与断屏恢复。Wayland 检查系统剪贴板集成，以及托盘恢复和手动隐藏。 |
| Android 网络与存储 | Wi-Fi、普通 VPN、Tailscale 切换前后检查发现及传输，覆盖两种保存目录与手动剪贴板。使用 WebView 108 检查界面及终端，使用低于 108 的 WebView 检查更新提示。 |
| 更新 | 用递增的签名版本验证桌面包、Linux-headless 版与 Android。覆盖活动任务延后安装、授权取消、错误签名拒绝、升级后数据保留及服务自启状态。 |
| 内存 | 用同样的正式构建比较空闲、关窗、关闭悬浮条、连续同步大图与传输大文件。记录应用及对应的 WebView 进程，Linux-headless 版记录服务进程。 |

## 图标与文档截图

标志源图为 [`frontend/public/meshshare.svg`](../frontend/public/meshshare.svg)。生成图标后按 [Tauri 配置](../backend/tauri.conf.json)和 [Android 准备脚本](../.github/scripts/prepare_android_project.py)更新 `backend/icons/` 中对应资源。

```sh
npx --yes @tauri-apps/cli@2 icon frontend/public/meshshare.svg --output backend/target/meshshare-icon-generation
```

README 界面截图放在 [`.github/assets/screenshots/`](../.github/assets/screenshots/)。用当前前端和演示数据取图，桌面窗口为 1180×780 逻辑像素，手机网页视口为 390×930，均以 2 倍像素保存为 WebP。

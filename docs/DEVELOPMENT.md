# MeshShare 开发指南

## 环境准备

安装 Git、Rust stable（当前 Tauri 2 依赖链和 single-instance 插件要求 Rust 1.90 或更高）和 Node.js 22，准备对应系统的构建依赖。Windows 需要 MSVC 和 WebView2，macOS 需要 Xcode 命令行工具，Ubuntu 24.04 需要 WebKitGTK 4.1、Wayland 开发库和 Ayatana AppIndicator 等开发包。[Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/#system-dependencies)

Android 开发还需要 Android Studio 或 Android command-line tools、JDK、Android SDK Platform、Build Tools、NDK 和四个 Rust Android targets。CI Android 构建固定使用 NDK 27.2.12479018，本地构建也应显式设置 `NDK_HOME`，避免多个 NDK 版本被自动选错。当前 App 使用局域网明文 HTTP/WebSocket，因此 Android 发行构建必须保留 `usesCleartextTraffic`；该设置只适用于受信任局域网，不表示公网安全。

以下命令从仓库根目录执行。Tauri CLI 由 `npx` 按需运行。

```bash
npm --prefix frontend ci
npm --prefix frontend run build
```

浏览器临时加入的网页由桌面节点从 `frontend/dist` 构建产物提供。改动网页端后，重新构建前端并重启桌面程序，再刷新浏览器页面。

## 更新项目图标

项目标志和网页共用 `frontend/public/meshshare.svg`。修改源图后，从仓库根目录运行 Tauri CLI 生成各平台图标。

```bash
npx --yes @tauri-apps/cli@2 icon frontend/public/meshshare.svg --output backend/target/meshshare-icon-generation
```

将生成的 ICO、ICNS 和 `backend/tauri.conf.json` 列出的 PNG 更新到 `backend/icons/`。这些文件随仓库维护，日常构建直接使用。

同一命令还会产出移动端图标，按平台放在 `backend/icons/mobile/<platform>/` 下，避免和桌面图标混在一个目录。当前 Android 使用 `backend/icons/mobile/android/`：把产物中的 `android/` 目录整份更新到这里，由 `.github/scripts/prepare_android_project.py` 在 `tauri android init` 之后写入 `gen/android/app/src/main/res`。因此 `backend/gen` 可再生，不要手工修改生成工程里的图标。自适应图标由 `mipmap-anydpi-v26/ic_launcher.xml`、`values/ic_launcher_background.xml` 与各密度 `ic_launcher_foreground.png` 组成，minSdk 29 起始终生效。

## 启动与构建

启动桌面开发窗口。Tauri 会自动启动 Vite。

```bash
npx --yes @tauri-apps/cli@2 dev -- --locked
```

初始化 Android 工程并运行：

```bash
export JAVA_HOME="/path/to/jdk"
export ANDROID_HOME="/path/to/android-sdk"
export NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
npx --yes @tauri-apps/cli@2 android init --ci --skip-targets-install
python3 .github/scripts/prepare_android_project.py
npx --yes @tauri-apps/cli@2 android dev
```

构建 APK 或 AAB：

```bash
npx --yes @tauri-apps/cli@2 android build --apk --target aarch64
npx --yes @tauri-apps/cli@2 android build --aab --target aarch64
```

Android App 的本机 Rust 节点能力是文件传输和网络指标；它不提供浏览器网页入口，桌面托盘、悬浮监控条、开机自启、桌面自动更新和系统剪贴板轮询不会编译进 Android 运行路径。发送文件使用系统文件选择器，接收文件默认写入应用专属的 Download/MeshShare 目录。Android 验证至少包括启动/关闭节点、局域网 HTTP/WS 连接、双向文件校验、手动剪贴板操作、Wi‑Fi 切换和进程退出后的状态清理。

构建本地调试程序，产物位于 `backend/target/debug/`。

```bash
npx --yes @tauri-apps/cli@2 build --debug --no-bundle -- --locked
```

开发模式和调试构建不会自动写入系统登录启动项，避免把调试可执行文件注册为正式启动入口；调试界面仍显示自启动开关，切换时会提示开发构建不写入登录启动项。开机自启动需要在正式桌面安装包中验证：首次启动应默认注册，登录后主窗口保持隐藏并可从托盘打开，重复打开应用只激活已有窗口。Windows 桌面还应验证左键双击系统托盘图标可以在显示和隐藏主窗口之间切换，单击不切换且右键菜单仍可用。Windows NSIS/MSI 和 Linux deb/rpm 卸载钩子会清理自启动注册。

推送到 `main` 时，[GitHub Actions 检查工作流](../.github/workflows/ci.yml) 会运行前端检查、构建、Rust 格式与 lint，并在 Windows x64、macOS Intel、macOS Apple Silicon 和 Linux x64 上运行 Rust 回归测试。Pull Request 不触发该工作流，仍可按本页检查命令本地验证。

Windows MSVC 的 Rust 测试宿主会链接 Tauri 使用的原生对话框 API。`backend/build.rs` 会把 Common Controls v6 manifest 嵌入 Windows/MSVC 的所有可链接目标，并关闭 `tauri-build` 的默认应用 manifest 资源，避免测试宿主因加载旧版 `comctl32` 在启动前以 `STATUS_ENTRYPOINT_NOT_FOUND` 退出。

推送 `vMAJOR.MINOR.PATCH` tag 时，[GitHub Actions 发布工作流](../.github/workflows/release.yml) 将 tag 版本注入 Tauri 构建，用于安装包元数据和节点报告的版本。Release 工作流不重复运行代码检查，只执行签名校验、前端构建、安装包生成和发布所需的校验。工作流逐个上传 Windows x64、macOS Intel 和 Apple Silicon、Linux x64 的七个桌面安装包，以及一个签名的 Android arm64 APK、两个 macOS updater archive 和 `latest.json`，不上传独立的 `.sig` 文件；发布说明会在 LLM 内容后附加按平台生成的下载指导，核对 GitHub Release 的附件完整后发布为 prerelease。Tauri CLI 按运行器系统、架构和版本缓存，缓存未命中时从 Cargo 安装固定版本。

Android Release 构建使用 GitHub Actions Secrets `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS` 和 `ANDROID_KEY_PASSWORD`。`ANDROID_KEYSTORE_BASE64` 是 Android 发布 keystore 的 base64 内容，不能把 keystore 或密码提交到仓库；缺少任一 Secret 时，Android 发布 job 会失败并阻止 Release 发布。普通用户下载 `MeshShare_<版本>_android_arm64.apk` 直接安装；AAB 仍通过本地 Android 构建命令生成，用于应用商店或 `bundletool`，不上传到普通下载指引。

### 自动更新发布

更新包由 Tauri updater 签名。首次配置时，在仓库外生成一组 updater 密钥，并将公钥内容写入 `backend/tauri.conf.json`；私钥和密码只保存到 GitHub Actions Secrets `TAURI_SIGNING_PRIVATE_KEY` 与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，不能提交到仓库。

```bash
npx --yes @tauri-apps/cli@2 signer generate --write-keys ~/.tauri/meshshare-updater.key
```

将私钥文件中的完整单行 base64 值原样保存到 `TAURI_SIGNING_PRIVATE_KEY`，不要填写文件路径、密码、URL 编码值（例如 `%2B` 或 `%0A`）、引号或命令输出；密码单独保存到 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。工作流会在跨平台构建前检查该值的 base64 和 Tauri/minisign 私钥格式。若私钥丢失，必须重新生成密钥并同步更新 `backend/tauri.conf.json` 中的公钥，已发布版本不会接受新公钥。

Release 工作流会为 Windows NSIS/MSI、macOS `.app.tar.gz`、Linux AppImage/deb/rpm 生成 `.sig` 文件，并由 `.github/scripts/generate_updater_manifest.py` 将签名内容合并进 `latest.json`；签名文件只在 Actions 构建产物中使用，不作为公开 Release Asset。清单同时包含 `windows-x86_64-nsis`、`windows-x86_64-msi`、`darwin-*-app` 和 `linux-x86_64-{appimage,deb,rpm}` 条目，使 updater 能匹配当前安装介质。发布任务在签名校验完成、清单生成且所有安装包上传后才解除 Release 草稿状态，并保持 prerelease 标记；人工验证安装包后，在 GitHub Release 页面取消 prerelease 标记，版本才会进入 `releases/latest` 并允许客户端自动更新。已人工晋级为稳定版的 Release 不会被工作流重跑覆盖。

当前 macOS 发布使用 ad-hoc 签名，不需要 Apple Developer 账号。工作流通过 `APPLE_SIGNING_IDENTITY=-` 生成可在 Apple Silicon 上运行的包，并在上传前验证 `.app` 的 ad-hoc 签名和 DMG 校验；由于没有 Apple 公证，首次从浏览器下载后需要用户在「系统设置 → 隐私与安全性」中选择「仍要打开」，或手动移除下载隔离标记。以后如果启用 Developer ID，需要把工作流的 `MACOS_SIGNING_MODE` 扩展为证书签名、公证和 stapling，并补充 Apple 凭据，不能把 ad-hoc 包描述为已公证。

开发构建不会执行 GitHub 更新检查。

### Wayland 运行验收

在 Ubuntu GNOME Wayland 会话中，确认 `loginctl show-session <id> -p Type` 返回 `Type=wayland`，并记录是否存在 `WAYLAND_DISPLAY`、`DISPLAY` 和 `Xwayland` 进程。重点验证文字和 PNG 剪贴板的自动读取、远端写入、历史恢复、手动同步以及悬浮监控条的拖动、隐藏和托盘恢复。

MeshShare 不强制把整个桌面壳切换到 X11。剪贴板会优先使用 Wayland data-control，没有该协议时尝试 XWayland；两者都不可用时，界面应报告原因，设备间共享、历史和文件传输仍应可用。Wayland 不提供全局窗口坐标、强制置顶或点击穿透保证，因此这些行为只能在 X11 会话中作为强保证验收。

发布说明由 OpenAI 兼容接口根据上一版本到当前 tag 的提交记录生成，随后追加 `.github/release-download-guide.md` 模板；模板中的桌面与 Android APK 链接由 `.github/scripts/append_release_download_guide.py` 根据当前 tag 和仓库名生成。`LLM_API_KEY` 取自 Actions Secrets，`LLM_BASE_URL` 和 `LLM_MODEL_NAME` 可配置为 Actions Secrets 或 Variables。生成失败时发布任务等待重跑。

重跑失败任务会沿用原提交和已构建的安装包。修复涉及发布工作流或脚本时，需推送指向修复提交的版本 tag 来触发新的运行。如果发布任务报告有额外附件，先到对应 Release 核对并清理旧附件，再重跑发布任务。

## 检查

Rust 格式和 lint 检查需要 `rustfmt` 和 `clippy` 组件，可通过 `rustup component add rustfmt clippy` 安装。

```bash
npm --prefix frontend run check
npm --prefix frontend run build
cargo fmt --manifest-path backend/Cargo.toml --all -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --locked
```

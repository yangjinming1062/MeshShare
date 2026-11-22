# MeshShare 开发指南

## 环境准备

安装 Git、Rust stable（1.90 或更高）和 Node.js 22，并准备对应系统的构建依赖：Windows 需要 MSVC 和 WebView2，macOS 需要 Xcode 命令行工具，Ubuntu 24.04 需要 WebKitGTK 4.1、Wayland 开发库和 Ayatana AppIndicator。[Tauri 2 系统依赖](https://v2.tauri.app/start/prerequisites/#system-dependencies)

Android 开发还需要 Android Studio 或 command-line tools、JDK、SDK Platform、Build Tools、NDK 和四个 Rust Android targets。CI 固定使用 NDK 27.2.12479018，本地也应显式设置 `NDK_HOME`。发行构建必须保留 `usesCleartextTraffic`，因为 App 使用局域网明文 HTTP/WebSocket；该设置只适用于受信任局域网。

以下命令从仓库根目录执行，Tauri CLI 由 `npx` 按需运行。

```bash
npm --prefix frontend ci
npm --prefix frontend run build
```

网页端由桌面节点从 `frontend/dist` 提供；改动后重新构建并重启桌面程序，再刷新浏览器。

## 图标

标志源图是 `frontend/public/meshshare.svg`。修改后生成各平台图标：

```bash
npx --yes @tauri-apps/cli@2 icon frontend/public/meshshare.svg --output backend/target/meshshare-icon-generation
```

把 ICO、ICNS 和 `backend/tauri.conf.json` 列出的 PNG 更新到 `backend/icons/`；移动端图标放到 `backend/icons/mobile/<platform>/`，由 `.github/scripts/prepare_android_project.py` 写入生成的 Android 工程。`backend/gen` 可再生，不要手工修改其中的图标。自适应图标由 `mipmap-anydpi-v26/ic_launcher.xml`、`values/ic_launcher_background.xml` 和各密度前景图组成，minSdk 29 起生效。

## 启动与构建

```bash
npx --yes @tauri-apps/cli@2 dev -- --locked
```

初始化并运行 Android 工程：

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

`prepare_android_project.py` 把网络适配器、存储插件、更新插件和安装权限写入可再生的 `backend/gen/android`，插件源码在 `backend/android/`。APK 前端以 Android 10 自带的 Chromium 74 WebView 为目标，入口补齐了 Svelte 所需的 `String.replaceAll`。

调试构建产物位于 `backend/target/debug/`：

```bash
npx --yes @tauri-apps/cli@2 build --debug --no-bundle -- --locked
```

## 检查

需要 `rustfmt` 和 `clippy`，可通过 `rustup component add rustfmt clippy` 安装。

```bash
npm --prefix frontend run check
npm --prefix frontend run build
npm --prefix frontend test
cargo fmt --manifest-path backend/Cargo.toml --all -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --locked
```

在已解锁系统凭据库的桌面登录会话中，可运行真实密钥保存与恢复检查。检查使用独立的临时身份，结束后清理凭据。

```bash
cargo test --manifest-path backend/Cargo.toml --locked native_vault_restores_device_identity_and_signed_grant -- --ignored
```

推送到 `main` 时，[CI](../.github/workflows/ci.yml) 在桌面平台运行回归检查，并构建 Android 调试 APK。Pull Request 不触发这套检查，请在本地执行上述命令。

## 发布

推送 `vMAJOR.MINOR.PATCH` tag 触发 [发布工作流](../.github/workflows/release.yml)。工作流把 tag 版本注入安装包元数据和节点版本，不重复代码检查，只做签名校验、前端构建、安装包生成和发布校验。

| 产物 | 说明 |
| --- | --- |
| 桌面安装包 | Windows x64、macOS Intel/Apple Silicon、Linux x64，共七个 |
| Android | 签名的 arm64 APK；AAB 由本地构建，用于商店或 `bundletool` |
| 更新清单 | `latest.json`，含桌面平台条目和 `android` 字段；不单独上传 `.sig` |

发布说明由 OpenAI 兼容接口根据上一版本到当前 tag 的提交生成，再追加 `.github/release-download-guide.md` 模板；模板中的下载链接由 `.github/scripts/append_release_download_guide.py` 生成。`LLM_API_KEY` 来自 Actions Secrets，`LLM_BASE_URL` 和 `LLM_MODEL_NAME` 可配置为 Secrets 或 Variables；生成失败时发布任务等待重跑。

桌面更新包由 Tauri updater 签名。首次配置时在仓库外生成密钥，把公钥写入 `backend/tauri.conf.json`，私钥和密码只保存到 Secrets `TAURI_SIGNING_PRIVATE_KEY` 与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：

```bash
npx --yes @tauri-apps/cli@2 signer generate --write-keys ~/.tauri/meshshare-updater.key
```

私钥 Secret 只填文件里的单行 base64 值，不要填路径、密码、URL 编码、引号或命令输出。工作流在构建前校验格式；私钥丢失只能重新生成并同步公钥，已发布版本不会接受新公钥。签名文件只用于构建产物，不进入公开 Release。清单覆盖 NSIS、MSI、macOS `.app` 和 Linux AppImage/deb/rpm，使 updater 能匹配当前安装介质。发布任务在所有安装包上传后解除草稿并保持 prerelease；人工验证后取消 prerelease 才会进入 `releases/latest` 并允许客户端自动更新，已晋级的稳定版不会被重跑覆盖。

macOS 发布包使用 ad-hoc 签名；发布前校验 `.app` 签名与 DMG 完整性，不声明已公证。

Android 发布使用 Secrets `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS` 和 `ANDROID_KEY_PASSWORD`，缺任一都会使 job 失败并阻止发布。构建先用 `apksigner` 校验，再用 `aapt2 dump badging` 核对包名 `com.meshshare.android`、arm64、`versionName` 和 `versionCode`，并在 `latest.json` 的 `android` 字段记录 `size`、`sha256` 和 `version_code`。`versionCode` 为 `major * 1000000 + minor * 1000 + patch`，同一更新链使用同一发布证书。

重跑失败任务会沿用原提交和已构建的安装包。修复发布工作流或脚本时，需要推送指向修复提交的新版本 tag。若报告存在额外附件，先在对应 Release 清理旧附件再重跑。

## 文档截图

根 README 的界面图存放在 `.github/assets/screenshots/`。桌面图对应 1180×780 逻辑像素窗口，手机图对应 390×844 浏览器窄视口，均按 2 倍像素采集并保存为 WebP，单张不超过 300 KB。使用虚构设备名、示例网段、模拟指标和演示剪贴板内容，检查前景与弹窗不出现真实数据。

## 平台验收

源码检查不能替代安装包、真实设备与窗口管理器上的验证。

| 环境 | 操作与通过条件 |
| --- | --- |
| 桌面安装与登录 | 调试构建不注册登录项；正式包首次启动注册，登录后可从托盘打开，重复启动不增加实例。Windows 验证单击、双击与右键菜单；NSIS/MSI、deb/rpm 卸载后检查登录项清理。 |
| Tailscale | 两台桌面分处不同物理网络，验证单向端口可达时发现与双向剪贴板、双向可达时互传文件。断开再连接，不重复列出设备。macOS 从 Finder 启动，分别验证官方与 Homebrew CLI。另测同一 LAN 的 mDNS 与手动连接。 |
| 桌面终端 | Windows、macOS、Linux 各自验证本机 shell、中文输入、Tab、Ctrl+C、全屏程序和缩放。两台桌面通过 LAN 和 Tailscale 分别配对、重启、再次连接；核对两个并行标签的目录和输入彼此独立，隐藏主窗口不丢会话。验证拒绝配对、换钥、撤销和断网后的远端进程回收，以及活动终端延后更新。 |
| Android 网络与存储 | 真机验证打开、退出、切换 Wi‑Fi、双向文件校验、两种保存目录及手动剪贴板。普通 VPN 要覆盖先开 VPN 再开 App、运行中切换与两条 `/1` 分流路由；核对发现和监听重建。另测 Tailscale、无物理网络与 VPN lockdown 下的降级。 |
| Windows / macOS / X11 悬浮条 | 同屏全屏约半秒隐藏，退出约一秒恢复；覆盖另一屏、部分与完全遮挡、最大化、虚拟桌面及启动时已全屏。验证位置、穿透、焦点与手动隐藏偏好；检测失败不刷日志，退出后停止检测。 |
| 多屏与系统差异 | 测负坐标、混合 DPI、断开显示器、跨屏拖动。Windows 从 `(-32000, -32000)` 和运行中离屏恢复，缩放后两行均可见；macOS 覆盖全屏 Space、刘海屏及无预留工作区，不新增权限请求；X11 长时间切换后文件描述符与连接数稳定。 |
| Wayland | 在 GNOME 用 `loginctl show-session <id> -p Type` 确认会话类型，记录 XWayland 状态。分别测文字、PNG、历史恢复及系统剪贴板不可用时的节点共享；悬浮条可拖动、手动隐藏与托盘恢复，全屏不触发自动隐藏。 |
| Linux 更新 | 用连续签名版本测试：系统包授权后安装，取消或缺少 polkit 代理时保持待安装，24 小时内不重复请求；手动检查可重试。验证活动传输延后安装、安装期间不接新任务，以及 AppImage 更新。 |
| Android 更新 | 用同一测试证书签名的两个递增版本验证覆盖安装和数据保留。Android 10/11 检查系统弹窗，12 及以上检查免确认及回退；覆盖计费网络、断网、活跃传输、拒绝授权与错误签名或版本。 |

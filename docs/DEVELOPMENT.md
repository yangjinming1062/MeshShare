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

`prepare_android_project.py` 把网络适配器、存储插件、更新插件和安装权限写入可再生的 `backend/gen/android`，插件源码在 `backend/android/`。APK 前端以 Android 10 自带的 Chromium 74 WebView 为目标，入口补齐了 Svelte 所需的 `String.replaceAll`。发送使用系统文件选择器，接收默认写入应用私有目录，授予「所有文件访问权限」后写入公共 `Download/MeshShare`。

Android 验收覆盖启动与关闭、局域网 HTTP/WS 双向传输与校验、手动剪贴板操作、Wi‑Fi 切换、普通全隧道 VPN（含用 `0.0.0.0/1` 与 `128.0.0.0/1` 代替默认路由的代理类 VPN）在「先开 VPN 再开 App」和「运行中开关」两种顺序下的 mDNS/HTTP/WS 发现、Tailscale 连接、监听器重建和退出清理。VPN lockdown 或无物理网络时记录系统拒绝绑定的降级结果。

调试构建产物位于 `backend/target/debug/`：

```bash
npx --yes @tauri-apps/cli@2 build --debug --no-bundle -- --locked
```

开发模式和调试构建不会写入系统登录启动项；调试界面仍显示开关，切换时提示该行为不生效。开机自启需在正式安装包中验证：首次启动默认注册，登录后主窗口隐藏且可从托盘打开，重复启动只激活已有窗口；Windows 还应确认左键双击托盘切换显隐、单击不切换、右键菜单可用。Windows NSIS/MSI 和 Linux deb/rpm 卸载钩子会清理自启动注册。

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

推送到 `main` 时，[CI 工作流](../.github/workflows/ci.yml) 运行前端检查、构建、Rust 格式与 lint，在 Windows x64、macOS Intel、macOS Apple Silicon 和 Linux x64 上运行回归测试，并构建调试用的 Android APK；Pull Request 不触发，需要本地按上述命令验证。Android 构建由 `.github/scripts/gradle-mirror.init.gradle` 在 `GRADLE_USER_HOME/init.d` 注入 Maven Central 镜像，避免共享 runner 的出口 IP 被限流（HTTP 429），本地构建不加载该脚本。Windows MSVC 的测试宿主会链接 Tauri 的原生对话框 API，因此 `backend/build.rs` 把 Common Controls v6 manifest 嵌入所有可链接目标，并关闭 `tauri-build` 的默认应用 manifest，避免测试宿主在启动前以 `STATUS_ENTRYPOINT_NOT_FOUND` 退出。

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

macOS 当前使用 ad-hoc 签名，不需要 Apple Developer 账号；工作流以上传前验证 `.app` 签名和 DMG 校验。由于没有公证，首次从浏览器下载后需要用户在「隐私与安全性」中选择「仍要打开」。以后启用 Developer ID 时，需要扩展 `MACOS_SIGNING_MODE` 为证书签名、公证和 stapling，不能把 ad-hoc 包描述为已公证。

Linux 系统包由 `pkexec` 调用 `dpkg -i` 或 `rpm -U`，授权对话框由 polkit 弹出。验收时确认：授权后安装并重启到新版本；取消授权或无授权代理时显示「待安装」并在 24 小时内不重复请求，点「立即检查」可立即重试；有传输时先等传输完成，安装窗口内的新请求提示稍后重试。AppImage 首次运行前需要 `chmod +x`，系统包版本运行时先退出托盘，否则只会激活已有窗口。端到端授权更新要求两个连续的签名版本，不能在本机用未签名测试包替代。

Android 发布使用 Secrets `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASSWORD`、`ANDROID_KEY_ALIAS` 和 `ANDROID_KEY_PASSWORD`，缺任一都会使 job 失败并阻止发布。构建先用 `apksigner` 校验，再用 `aapt2 dump badging` 核对包名 `com.meshshare.android`、arm64、`versionName` 和 `versionCode`，并在 `latest.json` 的 `android` 字段记录 `size`、`sha256` 和 `version_code`。`versionCode` 为 `major * 1000000 + minor * 1000 + patch`，同一更新链使用同一发布证书。晋级稳定版前，用同一测试密钥签名的两个递增版本验证覆盖安装：Android 10/11 确认系统弹窗，Android 12 及以上确认免确认与回退路径；复查数据保留、活跃传输延后、取消或拒绝授权后的恢复、计费网络、断网重试，以及错误签名或版本被拒绝。

重跑失败任务会沿用原提交和已构建的安装包。修复发布工作流或脚本时，需要推送指向修复提交的新版本 tag。若报告存在额外附件，先在对应 Release 清理旧附件再重跑。

## 文档截图

根 README 的界面图存放在 `.github/assets/screenshots/`。桌面图对应 1180×780 逻辑像素窗口，手机图对应 390×844 浏览器窄视口，均按 2 倍像素采集并保存为 WebP，单张不超过 300 KB。使用虚构设备名、示例网段、模拟指标和演示剪贴板内容，检查前景与弹窗不出现真实数据。

## 悬浮条全屏验收

在 Windows、macOS 和 Linux X11 分别验证浏览器全屏视频、播放器与无边框游戏：同屏约半秒内隐藏，退出全屏或最小化后约一秒内恢复，普通最大化不隐藏。双屏时把焦点切到另一屏，全屏画面仍可见则保持隐藏；另一屏全屏不得隐藏本屏悬浮条。普通窗口仅部分遮挡全屏应用时仍隐藏，完全遮挡后恢复，切换虚拟桌面也应重新判断。

验证启动时保存位置所在屏幕已有全屏画面时，悬浮条保持隐藏。检查隐藏前后的拖动位置和点击穿透，确认自动隐藏期间手动关闭显示后，退出全屏也不会重新显示。检查负坐标屏幕、混合 DPI/Retina、拖动跨屏、显示器断开及分辨率变化。macOS 额外检查刘海屏、全屏 Space 与没有菜单栏或 Dock 预留区的副屏。工作区与整屏重合等无法区分最大化的情况应保持显示，且不出现新的系统权限请求。X11 长时间运行并反复切换全屏、查询设置，确认 X server 连接数和进程文件描述符数保持稳定。

检查检测失败日志不重复刷屏，持续失败解除临时隐藏后仍可通过托盘控制；退出程序后检测线程应停止。纯状态与遮挡回归位于后端模块内，跨平台 CI 验证条件编译；真实全屏、窗口管理器和硬件组合仍须在目标平台手工验收，不能用静态检查替代。

## Wayland 验收

在 Ubuntu GNOME Wayland 会话确认 `loginctl show-session <id> -p Type` 返回 `Type=wayland`，记录 `WAYLAND_DISPLAY`、`DISPLAY` 和 Xwayland 状态。验证文字与 PNG 剪贴板的自动读取、远端写入、历史恢复、手动同步，以及悬浮条的拖动、隐藏和托盘恢复。

MeshShare 不强制整个桌面壳切换到 X11。剪贴板优先 Wayland data-control，没有该协议时尝试 XWayland；两者都不可用时界面报告原因，设备间共享、历史和文件传输仍应可用。Wayland 不提供全局坐标、强制置顶和点击穿透的强保证，这些行为只在 X11 会话验收；Wayland 全屏应用不应触发自动隐藏。

## 更新代理验收

把「更新代理」设为 `http://127.0.0.1:9` 后点「立即检查」，应显示连接失败原因和处置提示；改成可用代理后应检查成功，代理端能看到清单和安装包请求。

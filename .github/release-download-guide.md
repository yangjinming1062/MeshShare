## 下载指导

按设备的系统和处理器选择安装包。

| 系统 | 安装包 | 其他格式或要求 |
| --- | --- | --- |
| Windows x64 | [NSIS 安装程序（`.exe`）]({{WINDOWS_NSIS_URL}}) | [MSI 安装程序]({{WINDOWS_MSI_URL}}) |
| macOS Apple 芯片 | [DMG 安装包]({{MACOS_ARM64_DMG_URL}}) | |
| macOS Intel | [DMG 安装包]({{MACOS_X64_DMG_URL}}) | |
| Linux 桌面 x64 | [Debian / Ubuntu（`.deb`）]({{LINUX_DEB_URL}}) | [RPM]({{LINUX_RPM_URL}}) · [AppImage]({{LINUX_APPIMAGE_URL}}) |
| Linux-headless 版（amd64） | [系统服务（`.deb`）]({{HEADLESS_DEB_URL}}) | 以 Ubuntu 22.04 为构建环境，需要 systemd 249 及以上 |
| Android arm64 | [APK 安装包]({{ANDROID_ARM64_APK_URL}}) | Android 10 及以上、系统 WebView 108 及以上 |

- **macOS**。在「关于本机」确认芯片，把 DMG 中的 MeshShare 拖到「应用程序」。安装包使用 ad-hoc 签名。确认来源后，若 macOS 拦截打开，可在「系统设置 → 隐私与安全性」选择「仍要打开」。
- **Linux AppImage**。赋予下载的文件执行权限后打开。若 MeshShare 正在运行，先从托盘退出。
- **Android**。打开下载的 APK，按系统提示允许该来源安装应用。

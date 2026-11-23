## 下载指导

请根据系统和处理器架构选择对应的安装包：

| 系统 | 推荐下载 | 其他格式 |
| --- | --- | --- |
| Windows x64 | [NSIS 安装程序（`.exe`）]({{WINDOWS_NSIS_URL}}) | [MSI 安装程序]({{WINDOWS_MSI_URL}}) |
| macOS Apple Silicon | [DMG 安装包]({{MACOS_ARM64_DMG_URL}}) | — |
| macOS Intel | [DMG 安装包]({{MACOS_X64_DMG_URL}}) | — |
| Linux x64 | [`.deb`]({{LINUX_DEB_URL}})（Debian/Ubuntu） | [`.rpm`]({{LINUX_RPM_URL}}) · [AppImage]({{LINUX_APPIMAGE_URL}}) |
| Linux NAS / 无桌面 amd64 | [headless 系统服务（`.deb`）]({{HEADLESS_DEB_URL}}) | glibc 2.35、systemd 249 或更高版本 |
| Android arm64 | [APK 安装包]({{ANDROID_ARM64_APK_URL}}) | — |

- macOS：在「关于本机」确认芯片，把 DMG 中的应用拖到「应用程序」。安装包为 ad-hoc 签名、未经公证；确认来自本项目 Release 后，可在「系统设置 → 隐私与安全性」选择「仍要打开」。不要关闭系统 Gatekeeper。
- Linux：AppImage 需要执行权限（`chmod +x MeshShare_*.AppImage`）。若已运行系统包版本，先从托盘退出，再打开 AppImage。
- Android：下载 APK 后，按系统提示允许该来源安装应用。

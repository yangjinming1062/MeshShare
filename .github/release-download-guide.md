## 下载指导

请根据系统和处理器架构选择对应的安装包：

| 系统 | 推荐下载 | 其他格式 |
| --- | --- | --- |
| Windows x64 | [NSIS 安装程序（`.exe`）]({{WINDOWS_NSIS_URL}}) | [MSI 安装程序]({{WINDOWS_MSI_URL}}) |
| macOS Apple Silicon | [DMG 安装包]({{MACOS_ARM64_DMG_URL}}) | — |
| macOS Intel | [DMG 安装包]({{MACOS_X64_DMG_URL}}) | — |
| Linux x64 | [`.deb`]({{LINUX_DEB_URL}})（Debian/Ubuntu） | [`.rpm`]({{LINUX_RPM_URL}}) · [AppImage]({{LINUX_APPIMAGE_URL}}) |
| Android arm64 | [APK 安装包]({{ANDROID_ARM64_APK_URL}}) | — |

- Windows：普通用户下载 NSIS `.exe`；需要 MSI 的企业部署场景选择 MSI。
- macOS：在“关于本机”确认芯片后选择对应 DMG。
- Linux：Debian/Ubuntu 选择 `.deb`，Fedora/RHEL/openSUSE 选择 `.rpm`；系统包版本的自动更新会在本机弹出一次系统授权确认。AppImage 下载后先执行 `chmod +x MeshShare_*.AppImage` 再运行，浏览器下载不带执行权限；已经安装系统包并且正在运行时，先从托盘退出再打开 AppImage，否则只会激活已运行的窗口。
- Android：大多数近年手机选择 arm64 APK。安装时按系统提示允许该来源安装应用。已有「应用更新」入口的版本可以在 App 内直接更新。AAB 用于应用商店或 `bundletool`。

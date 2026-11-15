## 下载指导

请根据系统和处理器架构选择对应的安装包：

| 系统 | 推荐下载 | 其他格式 |
| --- | --- | --- |
| Windows x64 | [NSIS 安装程序（`.exe`）]({{WINDOWS_NSIS_URL}}) | [MSI 安装程序]({{WINDOWS_MSI_URL}}) |
| macOS Apple Silicon | [DMG 安装包]({{MACOS_ARM64_DMG_URL}}) | — |
| macOS Intel | [DMG 安装包]({{MACOS_X64_DMG_URL}}) | — |
| Linux x64 | [AppImage]({{LINUX_APPIMAGE_URL}}) | [`.deb`]({{LINUX_DEB_URL}}) · [`.rpm`]({{LINUX_RPM_URL}}) |
| Android arm64 | [APK 安装包]({{ANDROID_ARM64_APK_URL}}) | — |

- Windows：普通用户下载 NSIS `.exe`；需要 MSI 的企业部署场景选择 MSI。
- macOS：在“关于本机”确认芯片后选择对应 DMG。
- Linux：AppImage 无需安装；Debian/Ubuntu 选择 `.deb`，Fedora/RHEL/openSUSE 选择 `.rpm`。
- Android：大多数近年手机选择 arm64 APK；下载后允许安装此来源的应用，再打开 APK 完成安装。该 APK 由项目发布密钥签名，可直接安装；AAB 仅用于应用商店或 `bundletool`，不作为普通下载入口。

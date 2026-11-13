# MeshShare

MeshShare 是一个去中心化的局域网跨设备共享工具。每台桌面设备都是平等节点：它们自动发现彼此，复制到一台电脑的文字或图片剪贴板可以直接在另一台电脑粘贴，文件和文件夹则由发送方直接流式传给接收方。

## 使用方式

桌面程序启动后会显示主窗口并常驻系统托盘。主窗口列出当前在线的桌面设备：

- 点击设备卡片选择文件或文件夹发送。
- 把文件或文件夹拖到设备卡片上发送。
- 接收端确认并选择目录后，文件直接写入目标电脑。
- 关闭窗口只隐藏窗口，服务仍在后台运行；从托盘菜单选择“退出程序”才会停止节点。

主窗口中的“手机扫码访问”区域会列出所有可访问地址，例如物理局域网地址和 Tailscale 地址。切换地址后二维码会同步更新。手机扫描二维码进入 `/mobile` 页面，可以查看在线设备、编辑共享剪贴板并向指定桌面节点发送文件。

## 开发

开发环境需要 Rust、Node.js 和 Tauri 2 依赖：

```bash
cd web
npm install
npm run dev
```

另一个终端启动桌面壳：

```bash
cargo install tauri-cli --version '^2'
cargo tauri dev --manifest-path src-tauri/Cargo.toml
```

前端静态构建和检查：

```bash
npm --prefix web run check
npm --prefix web run build
```

Rust 检查：

```bash
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

默认节点服务监听 `37880`，如果端口被占用会自动选择空闲端口。服务只面向受信任局域网，不提供账号、配对或公网安全边界；不要把端口直接暴露到互联网。

架构、协议和体验约束见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)、[`docs/ARCHITECTURE_MAP.md`](docs/ARCHITECTURE_MAP.md)、[`docs/PROTOCOL.md`](docs/PROTOCOL.md) 和 [`docs/DESIGN.md`](docs/DESIGN.md)。

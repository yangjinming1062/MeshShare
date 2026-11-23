// 经典脚本在应用模块解析之前运行，低版本 Android WebView 也能显示更新提示。
var chromium = /Chrome\/(\d+)/.exec(navigator.userAgent);
if ('__TAURI_INTERNALS__' in window && /Android/i.test(navigator.userAgent) && chromium && Number(chromium[1]) < 108) {
  document.documentElement.setAttribute('data-update-webview', '');
}

// 窗口样式在应用模块加载前生效，包括 Vite 无法编译入口时。
if (new URLSearchParams(location.search).get('window') === 'monitor') {
  document.documentElement.dataset.window = 'monitor';
}

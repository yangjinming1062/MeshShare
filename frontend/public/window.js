// 经典脚本在应用模块解析前检测 Android WebView，并为低于 108 的版本启用更新提示。
var chromium = /Chrome\/(\d+)/.exec(navigator.userAgent);
if ('__TAURI_INTERNALS__' in window && /Android/i.test(navigator.userAgent) && chromium && Number(chromium[1]) < 108) {
  document.documentElement.setAttribute('data-update-webview', '');
}

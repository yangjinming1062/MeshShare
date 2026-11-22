// 窗口样式在应用模块加载前生效，包括 Vite 无法编译入口时。
if (new URLSearchParams(location.search).get('window') === 'monitor') {
  document.documentElement.dataset.window = 'monitor';
}

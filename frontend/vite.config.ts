import { defineConfig, type Plugin } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// xterm 6.0.0 的 DOM 渲染器通过 style 属性写入真彩色，打包后的 CSP 会拦截。
const xtermStyleMethod = '_addStyle(t,e){t.setAttribute("style",`${t.getAttribute("style")||""}${e};`)}';
const cssomStyleMethod = `_addStyle(element, declaration) {
  const separator = declaration.indexOf(':');
  element.style.setProperty(declaration.slice(0, separator), declaration.slice(separator + 1));
}`;

export function patchXtermColorStyles(source: string): string {
  const start = source.indexOf(xtermStyleMethod);
  if (start === -1 || start !== source.lastIndexOf(xtermStyleMethod)) {
    throw new Error('无法定位 xterm 6.0.0 的 DOM 样式写入方法，请核对真彩色 CSP 补丁');
  }
  return source.replace(xtermStyleMethod, cssomStyleMethod);
}

const xtermCssom: Plugin = {
  name: 'xterm-cssom',
  enforce: 'pre',
  transform(source, id) {
    if (id.split('?')[0].endsWith('/@xterm/xterm/lib/xterm.mjs')) {
      return { code: patchXtermColorStyles(source), map: null };
    }
  },
};

export default defineConfig({
  plugins: [svelte(), xtermCssom],
  // 开发服务器直接转换 xterm 的 ESM，和正式构建共用补丁。
  optimizeDeps: { exclude: ['@xterm/xterm'] },
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: 'es2022',
    rollupOptions: { input: { index: 'index.html', monitor: 'monitor.html' } },
  },
});

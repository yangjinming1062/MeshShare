import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

export function createRenderer(container: HTMLElement) {
  const term = new Terminal({
    cursorBlink: true,
    disableStdin: true,
    fontFamily: '"Cascadia Mono", Menlo, Consolas, monospace',
    fontSize: 14,
    scrollback: 3000,
    theme: { background: '#141923', foreground: '#edf0f6', cursor: '#f6f8ff' },
  });
  // 终端输出不触发系统剪贴板读写，也不从受信任的 WebView 打开链接。
  term.parser.registerOscHandler(52, () => true);
  term.parser.registerOscHandler(8, () => true);
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(container);
  return { term, fit };
}

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import { patchXtermColorStyles } from '../vite.config.ts';

const boot = readFileSync(new URL('../public/window.js', import.meta.url), 'utf8');

test('the packaged WebView permits xterm style elements and restricts scripts and style attributes', () => {
  const { app: { security } } = JSON.parse(readFileSync(new URL('../../backend/tauri.conf.json', import.meta.url), 'utf8'));
  const directives = new Map<string, string[]>(security.csp.split(';').map((directive: string) => {
    const [name, ...sources] = directive.trim().split(/\s+/);
    return [name, sources];
  }));

  // Tauri 注入样式 nonce 后，WebView 会忽略 'unsafe-inline'。
  assert.deepEqual(security.dangerousDisableAssetCspModification, ['style-src']);
  assert.deepEqual(directives.get('style-src'), ["'self'", "'unsafe-inline'"]);
  assert.deepEqual(directives.get('style-src-attr'), ["'none'"]);
  assert.deepEqual(directives.get('script-src'), ["'self'"]);
});

test('xterm applies RGB foreground and background through CSSOM on the same cell', () => {
  const bundle = readFileSync(new URL(import.meta.resolve('@xterm/xterm/lib/xterm.mjs')), 'utf8');
  const method = patchXtermColorStyles(bundle).match(/_addStyle\([^)]*\)\s*\{[^}]*\}/)?.[0];
  assert.ok(method);
  const { _addStyle } = runInNewContext(`({${method}})`);

  const styles = new Map([['letter-spacing', '1px']]);
  const element = { style: { setProperty: (name: string, value: string) => styles.set(name, value) } };
  _addStyle(element, 'background-color:#2060c0');
  _addStyle(element, 'color:#f05420');
  assert.deepEqual([...styles], [
    ['letter-spacing', '1px'],
    ['background-color', '#2060c0'],
    ['color', '#f05420'],
  ]);
});

test('the classic startup script can display the Android WebView upgrade message without mounting Svelte', () => {
  for (const [version, native, blocked] of [[74, true, true], [107, true, true], [108, true, false], [120, true, false], [107, false, false]]) {
    let askedForUpdate = false;
    runInNewContext(boot, {
      navigator: { userAgent: 'Mozilla/5.0 (Linux; Android 10) Chrome/' + version + '.0 Mobile' },
      window: native ? { __TAURI_INTERNALS__: {} } : {},
      document: { documentElement: { setAttribute(name: string) { askedForUpdate = name === 'data-update-webview'; } } },
      location: { search: '' },
      URLSearchParams,
    });
    assert.equal(askedForUpdate, blocked);
  }
});

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const boot = readFileSync(new URL('../public/window.js', import.meta.url), 'utf8');

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

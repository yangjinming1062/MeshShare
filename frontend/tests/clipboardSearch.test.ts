import assert from 'node:assert/strict';
import test from 'node:test';
import { filterClipboardHistory, fuzzyScore } from '../src/clipboardSearch.ts';
import type { ClipboardHistoryItem } from '../src/types.ts';

function item(overrides: Partial<ClipboardHistoryItem> & { event_id: string }): ClipboardHistoryItem {
  return {
    source_node_id: 'node',
    source_name: '演示笔记本',
    updated_at_ms: 0,
    recorded_at_ms: 0,
    has_text: true,
    has_image: false,
    size_bytes: 10,
    ...overrides,
  };
}

test('fuzzy score matches subsequences and ignores case', () => {
  assert.notEqual(fuzzyScore('pjc', 'Project clipboard'), null);
  assert.equal(fuzzyScore('xyz', 'Project clipboard'), null);
  assert.equal(fuzzyScore('ABC', 'abc'), fuzzyScore('abc', 'abc'));
});

test('every query token must match', () => {
  assert.notEqual(fuzzyScore('会议 记录', '下午会议记录'), null);
  assert.equal(fuzzyScore('会议 照片', '下午会议记录'), null);
});

test('empty query keeps the original list and unmatched queries return empty', () => {
  const items = [item({ event_id: 'a', text_preview: 'one' }), item({ event_id: 'b', text_preview: 'two' })];
  assert.equal(filterClipboardHistory(items, '   '), items);
  assert.deepEqual(filterClipboardHistory(items, 'zzz'), []);
});

test('matches text, source device and content type', () => {
  const items = [
    item({ event_id: 'text', text_preview: '接口返回 500', updated_at_ms: 2 }),
    item({ event_id: 'image', has_text: false, has_image: true, text_preview: undefined, source_name: '演示设计机', updated_at_ms: 1 }),
  ];
  assert.deepEqual(filterClipboardHistory(items, '接口').map((entry) => entry.event_id), ['text']);
  assert.deepEqual(filterClipboardHistory(items, '设计机').map((entry) => entry.event_id), ['image']);
  assert.deepEqual(filterClipboardHistory(items, '图').map((entry) => entry.event_id), ['image']);
});

test('contiguous and prefix matches rank above scattered ones', () => {
  const items = [
    item({ event_id: 'scattered', text_preview: 'a-b-c', updated_at_ms: 3 }),
    item({ event_id: 'after-space', text_preview: '记录 abc', updated_at_ms: 2 }),
    item({ event_id: 'prefix', text_preview: 'abc 记录', updated_at_ms: 1 }),
  ];
  assert.deepEqual(filterClipboardHistory(items, 'abc').map((entry) => entry.event_id), [
    'prefix',
    'after-space',
    'scattered',
  ]);
});

test('equal scores keep the newest record first', () => {
  const items = [
    item({ event_id: 'older', text_preview: 'abc', updated_at_ms: 1 }),
    item({ event_id: 'newer', text_preview: 'abc', updated_at_ms: 2 }),
  ];
  assert.deepEqual(filterClipboardHistory(items, 'abc').map((entry) => entry.event_id), ['newer', 'older']);
});

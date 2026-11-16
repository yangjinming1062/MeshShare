import assert from 'node:assert/strict';
import test from 'node:test';
import { transferStatusText } from '../src/format.ts';

const base = {
  transfer_id: 'id',
  status: 'accepted' as const,
  completed_bytes: 0,
  total_bytes: 10,
  completed_items: 0,
  total_items: 1,
};

test('accepted transfers describe the sender and receiver separately', () => {
  assert.equal(transferStatusText({ ...base, direction: 'send' }, 'desktop'), '等待发送');
  assert.equal(transferStatusText({ ...base, direction: 'receive' }, 'android'), '准备接收');
  assert.equal(transferStatusText({ ...base, direction: 'receive' }, 'web'), '准备接收');
});

test('other transfer states keep their existing wording', () => {
  assert.equal(transferStatusText({ ...base, status: 'waiting' }, 'desktop'), '等待确认');
  assert.equal(transferStatusText({ ...base, status: 'cancelled' }, 'desktop'), '已取消');
});

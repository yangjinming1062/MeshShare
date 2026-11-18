import assert from 'node:assert/strict';
import test from 'node:test';
import { mergeTransferProgress } from '../src/transferProgress.ts';
import type { TransferProgress } from '../src/types';

const establishing: TransferProgress = {
  transfer_id: 'id',
  status: 'waiting',
  awaiting_offer_reply: true,
  completed_bytes: 0,
  total_bytes: 10,
  completed_items: 0,
  total_items: 1,
  direction: 'send',
};

test('an old start response cannot replace the receiver’s reply', () => {
  for (const status of ['waiting', 'accepted'] as const) {
    const reply = { ...establishing, status, awaiting_offer_reply: false };
    assert.deepEqual(mergeTransferProgress(establishing, reply), { ...reply, offer: undefined });
    assert.deepEqual(mergeTransferProgress(reply, establishing), { ...reply, offer: undefined });
  }
});

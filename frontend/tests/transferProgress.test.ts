import assert from 'node:assert/strict';
import test from 'node:test';
import { mergeTransferProgress, newestFinishedTransfer } from '../src/transferProgress.ts';
import type { TransferHistoryEntry, TransferProgress } from '../src/types';

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

test('the first loaded history is a baseline and a finished retry counts as new', () => {
  const failed: TransferHistoryEntry = {
    transfer_id: 'failed',
    direction: 'receive',
    peer_node_id: 'peer',
    peer_name: '电脑',
    status: 'failed',
    completed_bytes: 0,
    total_bytes: 10,
    completed_items: 0,
    total_items: 1,
    finished_at_ms: 1,
  };
  const retried = { ...failed, status: 'completed' as const, retry_count: 1, finished_at_ms: 2 };
  assert.equal(newestFinishedTransfer(null, [failed]), undefined);
  assert.equal(newestFinishedTransfer([], [failed]), failed);
  assert.equal(newestFinishedTransfer([failed], [{ ...failed }]), undefined);
  assert.equal(newestFinishedTransfer([failed], [retried]), retried);
  assert.equal(newestFinishedTransfer([retried], [failed]), undefined);
});

import assert from 'node:assert/strict';
import test from 'node:test';
import { mergeTransferProgress, newestFinishedTransfer } from '../src/transferProgress.ts';
import type { TransferHistoryEntry, TransferOffer, TransferProgress } from '../src/types';

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

test('a delayed snapshot restores the manifest without rolling back live progress', () => {
  const offer: TransferOffer = {
    transfer_id: 'id', source_node_id: 'source', source_name: '电脑', target_node_id: 'target',
    items: [{ item_id: 'file', name: 'file', relative_path: 'file', size: 10, sha256: '', media_type: 'text/plain', directory: false }],
    total_bytes: 10, created_at_ms: 1,
  };
  const live = { ...establishing, status: 'uploading' as const, awaiting_offer_reply: false, completed_bytes: 8 };
  const snapshot = { ...live, completed_bytes: 4, offer };
  for (const merged of [mergeTransferProgress(live, snapshot), mergeTransferProgress(snapshot, live)]) {
    assert.equal(merged.completed_bytes, 8);
    assert.equal(merged.offer, offer);
  }

  const failed = { ...live, status: 'failed' as const, error: '网络中断' };
  const retry = { ...establishing, retry_count: 1, awaiting_offer_reply: false, offer };
  for (const merged of [mergeTransferProgress(failed, retry), mergeTransferProgress(retry, failed)]) {
    assert.equal(merged.retry_count, 1);
    assert.equal(merged.completed_bytes, 0);
    assert.equal(merged.error, undefined);
    assert.equal(merged.offer, offer);
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

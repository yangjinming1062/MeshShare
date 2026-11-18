import assert from 'node:assert/strict';
import test from 'node:test';
import { formatHistoryTime, transferHistoryForSessionView, transferHistoryLocation, transferHistoryPathText, transferHistoryPeerText, transferHistoryStatusText, transferStatusText } from '../src/format.ts';

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

test('waiting for an offer reply is distinct from waiting for manual confirmation', () => {
  const outgoing = { ...base, status: 'waiting' as const, direction: 'send' as const };
  assert.equal(transferStatusText({ ...outgoing, awaiting_offer_reply: true }, 'desktop'), '正在建立传输');
  assert.equal(transferStatusText({ ...outgoing, awaiting_offer_reply: false }, 'android'), '等待确认');
  assert.equal(transferStatusText({ ...outgoing, direction: 'receive' }, 'web'), '等待确认');
});

const historyBase = {
  transfer_id: 'history',
  direction: 'receive' as const,
  peer_node_id: 'peer-node',
  peer_name: '书房电脑',
  status: 'completed' as const,
  completed_bytes: 10,
  total_bytes: 10,
  completed_items: 1,
  total_items: 1,
  finished_at_ms: 0,
};

test('transfer history describes both ends and the root path', () => {
  assert.equal(transferHistoryPeerText(historyBase), '来自 书房电脑');
  assert.equal(transferHistoryPeerText({ ...historyBase, direction: 'send' }), '发送到 书房电脑');
  assert.equal(transferHistoryPathText({ ...historyBase, destination: 'D:/下载' }), '保存到 D:/下载');
  assert.equal(transferHistoryPathText(historyBase), '保存位置未记录');
  assert.equal(
    transferHistoryPathText({ ...historyBase, direction: 'send', source_paths: ['D:/资料', 'D:/照片'] }),
    '来源 D:/资料 等 2 处',
  );
  assert.equal(transferHistoryPathText({ ...historyBase, direction: 'send' }), '来源未知');
});

test('history entries resolve to the containing directory and the item to select', () => {
  assert.deepEqual(transferHistoryLocation({ ...historyBase, destination: 'D:/下载' }), { directory: 'D:/下载' });
  assert.deepEqual(transferHistoryLocation({ ...historyBase, destination: 'D:/下载', item_root: '报告.docx' }), {
    directory: 'D:/下载',
    select: 'D:/下载/报告.docx',
  });
  assert.deepEqual(transferHistoryLocation({ ...historyBase, destination: 'D:\\下载\\', item_root: '照片' }), {
    directory: 'D:\\下载\\',
    select: 'D:\\下载\\照片',
  });
  assert.equal(transferHistoryLocation(historyBase), null);
  assert.deepEqual(
    transferHistoryLocation({ ...historyBase, direction: 'send', source_paths: ['C:\\资料\\报告.docx'] }),
    { directory: 'C:\\资料', select: 'C:\\资料\\报告.docx' },
  );
  assert.deepEqual(
    transferHistoryLocation({ ...historyBase, direction: 'send', source_paths: ['C:/资料'] }),
    { directory: 'C:/', select: 'C:/资料' },
  );
  assert.deepEqual(
    transferHistoryLocation({ ...historyBase, direction: 'send', source_paths: ['/report.txt'] }),
    { directory: '/', select: '/report.txt' },
  );
  assert.equal(transferHistoryLocation({ ...historyBase, direction: 'send' }), null);
});

test('session history is shown from the receiving page perspective', () => {
  const sessionEntry = { ...historyBase, session_id: 'session', direction: 'send' as const, peer_name: 'Android 浏览器' };
  const viewed = transferHistoryForSessionView(sessionEntry, 'desktop-node', '书房电脑');
  assert.equal(viewed.direction, 'receive');
  assert.equal(viewed.peer_node_id, 'desktop-node');
  assert.equal(viewed.peer_name, '书房电脑');
  assert.equal(transferHistoryForSessionView(historyBase, 'desktop-node', '书房电脑'), historyBase);
});

test('history timestamps label today, yesterday and older dates', () => {
  const now = new Date(2025, 10, 18, 10, 0).getTime();
  assert.equal(formatHistoryTime(new Date(2025, 10, 18, 9, 5).getTime(), now), '09:05');
  assert.equal(formatHistoryTime(new Date(2025, 10, 17, 23, 59).getTime(), now), '昨天 23:59');
  assert.equal(formatHistoryTime(new Date(2025, 10, 16, 8, 7).getTime(), now), '11-16 08:07');
  assert.equal(formatHistoryTime(new Date(2024, 11, 31, 18, 30).getTime(), now), '2024-12-31 18:30');
  assert.equal(formatHistoryTime(undefined, now), '');
});

test('transfer history status keeps the terminal state and failure reason', () => {
  assert.equal(transferHistoryStatusText(historyBase), '已完成 · 10 B');
  assert.equal(transferHistoryStatusText({ ...historyBase, status: 'rejected' }), '已拒绝');
  assert.equal(
    transferHistoryStatusText({ ...historyBase, status: 'failed', error: '网络中断' }),
    '网络中断',
  );
});

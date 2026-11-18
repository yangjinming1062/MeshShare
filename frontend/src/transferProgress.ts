import type { TransferHistoryEntry, TransferProgress } from './types';

/** 首次加载建立基线，之后按传输 ID 和重试代次识别新终态。 */
export function newestFinishedTransfer(
  previous: readonly TransferHistoryEntry[] | null,
  entries: readonly TransferHistoryEntry[],
): TransferHistoryEntry | undefined {
  if (!previous) return undefined;
  return entries.find((entry) => !previous.some((seen) => seen.transfer_id === entry.transfer_id
    && (seen.retry_count ?? 0) >= (entry.retry_count ?? 0)));
}

function transferStatusRank(progress: TransferProgress): number {
  if (progress.status === 'waiting') return progress.awaiting_offer_reply ? 0 : 1;
  if (progress.status === 'accepted') return 2;
  if (progress.status === 'uploading') return 3;
  if (progress.status === 'ready') return 4;
  if (progress.status === 'completed') return 6;
  return 5;
}

/** HTTP 响应和 WebSocket 事件可能乱序到达，接收端已回复的任务不能退回建立传输。 */
export function mergeTransferProgress(current: TransferProgress | undefined, next: TransferProgress): TransferProgress {
  if (!current) return next;
  const currentRetry = current.retry_count ?? 0;
  const nextRetry = next.retry_count ?? 0;
  const keepCurrent = currentRetry > nextRetry
    || (currentRetry === nextRetry && transferStatusRank(current) > transferStatusRank(next));
  const preferred = keepCurrent ? current : next;
  return {
    ...(keepCurrent ? next : current),
    ...preferred,
    awaiting_offer_reply: preferred.awaiting_offer_reply,
    offer: next.offer ?? current.offer,
  };
}

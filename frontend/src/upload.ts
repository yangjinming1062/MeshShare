import { ApiError, isRetryableHttpStatus, responseError } from './api.ts';
import type { NodeInfo, TransferItem, TransferOffer } from './types';

export function randomId(): string {
  // 非安全上下文（局域网 HTTP）也需要生成传输标识。
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export function buildOffer(source: NodeInfo, targetNodeId: string, files: File[]): TransferOffer {
  const items = files.map((file) => ({
    item_id: randomId(),
    name: file.name,
    relative_path: file.webkitRelativePath || file.name,
    size: file.size,
    sha256: '',
    media_type: file.type || 'application/octet-stream',
    directory: false,
  }));
  return {
    transfer_id: randomId(),
    source_node_id: source.node_id,
    source_name: source.node_name,
    target_node_id: targetNodeId,
    items,
    total_bytes: items.reduce((sum, item) => sum + item.size, 0),
    created_at_ms: Date.now(),
  };
}

export async function uploadItems(
  offer: TransferOffer,
  files: File[],
  signal: AbortSignal,
  send: (item: TransferItem, file: File) => Promise<void>,
  completed = new Set<string>(),
) {
  for (const [index, item] of offer.items.entries()) {
    signal.throwIfAborted();
    if (completed.has(item.item_id)) continue;
    const file = files[index];
    if (!file) throw new Error(`找不到待上传文件：${item.relative_path}`);
    await send(item, file);
    completed.add(item.item_id);
  }
}

/** 同一项目的 PUT 幂等。重试时仍绑定原请求的接收节点。 */
export async function uploadFileWithRetry(url: string, file: File, mediaType: string, nodeId: string, signal: AbortSignal): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    signal.throwIfAborted();
    try {
      const response = await fetch(url, {
        method: 'PUT', redirect: 'error', headers: { 'Content-Type': mediaType, 'X-MeshShare-Target': nodeId }, body: file, signal,
      });
      if (!response.ok) throw await responseError(response);
      return;
    } catch (error) {
      if (signal.aborted || attempt >= 3 || (error instanceof ApiError && !isRetryableHttpStatus(error.status))) throw error;
      await new Promise((resolve) => setTimeout(resolve, 800 * 2 ** attempt));
    }
  }
}

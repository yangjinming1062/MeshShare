import { ApiError, isRetryableHttpStatus, responseError } from './api';
import type { NodeInfo, TransferOffer } from './types';

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

export async function putFile(url: string, file: File, mediaType: string, signal?: AbortSignal): Promise<void> {
  const response = await fetch(url, {
    method: 'PUT', redirect: 'error', headers: { 'Content-Type': mediaType }, body: file, signal,
  });
  if (!response.ok) throw await responseError(response);
}

/** 接收节点对同一项目的 PUT 幂等，暂时断网或服务忙时可以重发。 */
export async function uploadFileWithRetry(url: string, file: File, mediaType: string): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await putFile(url, file, mediaType);
    } catch (error) {
      if (attempt >= 3 || (error instanceof ApiError && !isRetryableHttpStatus(error.status))) throw error;
      await new Promise((resolve) => window.setTimeout(resolve, 800 * 2 ** attempt));
    }
  }
}

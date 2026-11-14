import { invoke } from '@tauri-apps/api/core';
import type { ClipboardHistoryDetail, ClipboardHistoryResponse, ClipboardSnapshot, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, PeerInfo, TransferProgress, TransferStatus } from './types';

let baseUrl = '';

export async function initApi(): Promise<string> {
  try {
    baseUrl = await invoke<string>('node_base_url');
  } catch {
    baseUrl = window.location.origin;
    if (baseUrl === 'null' || baseUrl.startsWith('tauri:')) baseUrl = 'http://127.0.0.1:37880';
  }
  return baseUrl.replace(/\/$/, '');
}

export function apiUrl(path: string, target = baseUrl): string {
  return `${target.replace(/\/$/, '')}${path}`;
}

export function websocketUrl(target = baseUrl): string {
  return apiUrl('/api/v1/ws', target).replace(/^http/, 'ws');
}

export async function request<T>(path: string, options: RequestInit = {}, target = baseUrl): Promise<T> {
  const response = await fetch(apiUrl(path, target), {
    ...options,
    cache: 'no-store',
    headers: {
      ...(options.body && !(options.body instanceof FormData) ? { 'Content-Type': 'application/json' } : {}),
      ...(options.headers ?? {}),
    },
  });
  const httpError = `请求失败 (${response.status})`;
  const payload: unknown = await response.json().catch((cause: unknown) => {
    if (!response.ok) throw new Error(httpError, { cause });
    const isHtml = response.headers.get('content-type')?.startsWith('text/html');
    const message = isHtml
      ? '未连接到 MeshShare 节点，请启动桌面应用'
      : `节点返回了无效的 JSON (${path})`;
    throw new Error(message, { cause });
  });
  if (!response.ok) {
    const message = typeof payload === 'object' && payload !== null && 'error' in payload && typeof payload.error === 'string'
      ? payload.error
      : httpError;
    throw new Error(message);
  }
  return payload as T;
}

async function requestList<T>(path: string): Promise<T[]> {
  const payload = await request<unknown>(path);
  if (!Array.isArray(payload)) throw new Error(`节点返回的列表格式无效 (${path})`);
  return payload;
}

export const getInfo = () => request<NodeInfo>('/api/v1/info');
export const getPeers = () => requestList<PeerInfo>('/api/v1/peers');
export const getMetrics = () => request<HardwareMetrics | null>('/api/v1/metrics');
export const getClipboard = () => request<ClipboardSnapshot | null>('/api/v1/clipboard');
export const getClipboardHistory = () => request<ClipboardHistoryResponse>('/api/v1/clipboard/history');
export const getClipboardHistoryDetail = (eventId: string) => request<ClipboardHistoryDetail>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}`);
export const restoreClipboardHistory = (eventId: string) => request<ClipboardSnapshot>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}/restore`, { method: 'POST' });
export const clearClipboardHistory = () => request<{ cleared: number }>('/api/v1/clipboard/history', { method: 'DELETE' });
export const getTransfers = (status?: TransferStatus) => requestList<TransferProgress>(`/api/v1/transfers${status ? `?status=${encodeURIComponent(status)}` : ''}`);
export const createMobileSession = (label?: string) => request<MobileSession>('/api/v1/mobile/sessions', { method: 'POST', body: JSON.stringify({ label }) });
export const getMobileSessions = () => requestList<MobileSession>('/api/v1/mobile/sessions');
export const getMobileTransfers = (sessionId?: string) => requestList<MobileTransferView>(`/api/v1/mobile/transfers${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`);
export const mobileDownloadUrl = (transferId: string, itemId: string, sessionId: string, target = baseUrl) => apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/download?session_id=${encodeURIComponent(sessionId)}`, target);
export const completeMobileItem = (transferId: string, itemId: string, sessionId: string) => request<MobileTransferView>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/complete`, { method: 'POST', body: JSON.stringify({ session_id: sessionId }) });

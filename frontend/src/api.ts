import { invoke } from '@tauri-apps/api/core';
import type { ClipboardHistoryDetail, ClipboardHistoryResponse, ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, NodeSettings, NodeSettingsPatch, PeerInfo, TransferProgress, TransferStatus } from './types';

let baseUrl = '';

/** HTTP 状态码保留在错误上，调用方才能区分对端拒绝与网络不可达。 */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

export async function initApi(): Promise<string> {
  try {
    baseUrl = await invoke<string>('node_base_url');
  } catch {
    baseUrl = window.location.origin;
    if (baseUrl === 'null' || baseUrl.startsWith('tauri:')) baseUrl = 'http://127.0.0.1:37880';
  }
  return baseUrl.replace(/\/$/, '');
}

export function setApiTarget(target: string): string {
  baseUrl = target.replace(/\/$/, '');
  return baseUrl;
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
    if (!response.ok) throw new ApiError(httpError, response.status);
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
    throw new ApiError(message, response.status);
  }
  return payload as T;
}

async function requestList<T>(path: string, target = baseUrl): Promise<T[]> {
  const payload = await request<unknown>(path, {}, target);
  if (!Array.isArray(payload)) throw new Error(`节点返回的列表格式无效 (${path})`);
  return payload;
}

export const getInfo = (target = baseUrl) => request<NodeInfo>('/api/v1/info', {}, target);
export const getPeers = (target = baseUrl) => requestList<PeerInfo>('/api/v1/peers', target);
export const getMetrics = (target = baseUrl) => request<HardwareMetrics | null>('/api/v1/metrics', {}, target);
export const getClipboard = (target = baseUrl) => request<ClipboardSnapshot | null>('/api/v1/clipboard', {}, target);
export const getClipboardStatus = (target = baseUrl) => request<ClipboardStatus>('/api/v1/clipboard/status', {}, target);
export const getSettings = (target = baseUrl) => request<NodeSettings>('/api/v1/settings', {}, target);
export const patchSettings = (patch: NodeSettingsPatch, target = baseUrl) => request<NodeSettings>('/api/v1/settings', { method: 'PATCH', body: JSON.stringify(patch) }, target);
export const requestUpdateCheck = (target = baseUrl) => request<NodeSettings>('/api/v1/settings/update-check', { method: 'POST' }, target);
export const getClipboardHistory = (target = baseUrl) => request<ClipboardHistoryResponse>('/api/v1/clipboard/history', {}, target);
export const getClipboardHistoryDetail = (eventId: string, target = baseUrl) => request<ClipboardHistoryDetail>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}`, {}, target);
export const restoreClipboardHistory = (eventId: string, target = baseUrl) => request<ClipboardSnapshot>(`/api/v1/clipboard/history/${encodeURIComponent(eventId)}/restore`, { method: 'POST' }, target);
export const clearClipboardHistory = (target = baseUrl) => request<{ cleared: number }>('/api/v1/clipboard/history', { method: 'DELETE' }, target);
export const getTransfers = (status?: TransferStatus, target = baseUrl) => requestList<TransferProgress>(`/api/v1/transfers${status ? `?status=${encodeURIComponent(status)}` : ''}`, target);
export const dismissTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const dismissMobileTransfer = (transferId: string, target = baseUrl) => request<TransferProgress>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}`, { method: 'DELETE' }, target);
export const createMobileSession = (label?: string, target = baseUrl) => request<MobileSession>('/api/v1/mobile/sessions', { method: 'POST', body: JSON.stringify({ label }) }, target);
export const getMobileSessions = (target = baseUrl) => requestList<MobileSession>('/api/v1/mobile/sessions', target);
export const getMobileTransfers = (sessionId?: string, target = baseUrl) => requestList<MobileTransferView>(`/api/v1/mobile/transfers${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, target);
export const mobileDownloadUrl = (transferId: string, itemId: string, sessionId: string, target = baseUrl, options: { autoComplete?: boolean } = {}) => apiUrl(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/download?session_id=${encodeURIComponent(sessionId)}${options.autoComplete ? '&auto_complete=true' : ''}`, target);
export const completeMobileItem = (transferId: string, itemId: string, sessionId: string, target = baseUrl) => request<MobileTransferView>(`/api/v1/mobile/transfers/${encodeURIComponent(transferId)}/items/${encodeURIComponent(itemId)}/complete`, { method: 'POST', body: JSON.stringify({ session_id: sessionId }) }, target);

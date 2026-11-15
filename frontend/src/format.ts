import type { ClipboardHistoryItem, ClipboardHistorySettings, HardwareMetrics, PeerInfo, TransferProgress, TransferStatus, UpdateFrequency, UpdateStatus } from './types';

export const METRICS_STALE_AFTER_MS = 5_000;

export function formatSize(bytes: number): string {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 ** 2) return (bytes / 1024).toFixed(1) + ' KB';
  if (bytes < 1024 ** 3) return (bytes / 1024 ** 2).toFixed(1) + ' MB';
  return (bytes / 1024 ** 3).toFixed(1) + ' GB';
}

export function formatRate(value: number | null | undefined): string {
  if (value == null) return '—';
  if (value >= 1024 ** 2) return (value / 1024 ** 2).toFixed(1) + ' MB/s';
  if (value >= 1024) return (value / 1024).toFixed(1) + ' KB/s';
  return Math.round(value) + ' B/s';
}

export function formatRateCompact(value: number | null | undefined): string {
  if (value == null) return '—';
  if (value >= 1024 ** 2) return (value / 1024 ** 2).toFixed(1) + 'M';
  if (value >= 1024) return Math.round(value / 1024) + 'K';
  return Math.round(value) + 'B';
}

export function formatPercent(value: number | null | undefined): string {
  return value == null ? '—' : Math.round(value) + '%';
}

export function formatMemory(value: number | null | undefined): string {
  return value == null ? '—' : formatSize(value);
}

export function memoryPercent(value: HardwareMetrics | null | undefined): number | undefined {
  if (value?.memory_used_bytes == null || value.memory_total_bytes == null) return undefined;
  if (value.memory_total_bytes <= 0) return undefined;
  return value.memory_used_bytes / value.memory_total_bytes * 100;
}

export function gpuUtilization(value: HardwareMetrics | null | undefined): number | undefined {
  const values = (value?.gpus ?? []).map((gpu) => gpu.utilization_percent).filter((item): item is number => item != null);
  return values.length ? Math.max(...values) : undefined;
}

export function gpuMemoryPercent(value: HardwareMetrics | null | undefined): number | undefined {
  const values = (value?.gpus ?? [])
    .map((gpu) => (gpu.memory_used_bytes != null && gpu.memory_total_bytes ? gpu.memory_used_bytes / gpu.memory_total_bytes * 100 : undefined))
    .filter((item): item is number => item != null);
  return values.length ? Math.max(...values) : undefined;
}

export function isMetricsStale(value: HardwareMetrics | null | undefined): boolean {
  return value != null && Date.now() - value.sampled_at_ms > METRICS_STALE_AFTER_MS;
}

export function peerSummary(peer: PeerInfo): string {
  if (peer.metrics_stale) return '指标已过期';
  const value = peer.metrics;
  if (!value) return '指标采集中';
  return 'CPU ' + formatPercent(value.cpu_usage_percent)
    + ' · 内存 ' + formatPercent(memoryPercent(value))
    + ' · ↓ ' + formatRate(value.network_down_bytes_per_sec)
    + ' ↑ ' + formatRate(value.network_up_bytes_per_sec);
}

export function formatTime(value: number | undefined): string {
  return value ? new Date(value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
}

export function formatConnectedSince(since: number): string {
  const minutes = Math.max(0, Math.round((Date.now() - since) / 60000));
  if (minutes < 1) return '刚刚';
  if (minutes < 60) return minutes + ' 分钟';
  const hours = Math.round(minutes / 60);
  if (hours < 24) return hours + ' 小时';
  return Math.round(hours / 24) + ' 天';
}

export function transferItemsLabel(transfer: TransferProgress): string {
  const names = transfer.offer?.items.map((item) => item.name) ?? [];
  if (names.length) return names.slice(0, 2).join('、') + (names.length > 2 ? ' 等 ' + names.length + ' 项' : '');
  return transfer.total_items + ' 个项目';
}

export function transferItemKey(transferId: string, itemId: string): string {
  return transferId + ':' + itemId;
}

export function isActiveTransferStatus(status: TransferStatus): boolean {
  return status === 'waiting' || status === 'accepted' || status === 'uploading' || status === 'ready';
}

export function transferStatusText(transfer: TransferProgress, surface: 'desktop' | 'web' | 'android'): string {
  const ratio = transfer.total_bytes
    ? Math.max(0, Math.min(1, transfer.completed_bytes / transfer.total_bytes))
    : 0;
  if (transfer.status === 'waiting') return '等待确认';
  if (transfer.status === 'accepted') return '准备接收';
  if (transfer.status === 'ready') {
    if (surface === 'desktop') return '等待网页端保存';
    return surface === 'android' ? '正在写入应用目录' : '等待保存';
  }
  if (transfer.status === 'completed') return '已完成 · ' + formatSize(transfer.total_bytes);
  if (transfer.status === 'rejected') return '已拒绝';
  if (transfer.status === 'cancelled') return '已取消';
  if (transfer.status === 'failed') return transfer.error ?? '失败';
  return formatSize(transfer.completed_bytes) + ' / ' + formatSize(transfer.total_bytes)
    + ' · ' + Math.round(ratio * 100) + '%';
}

export function clipboardHistoryPreview(item: ClipboardHistoryItem): string {
  if (item.text_preview) return item.text_preview;
  return item.has_image ? 'PNG 图片' : '空内容';
}

export function clipboardHistoryKind(item: ClipboardHistoryItem): string {
  if (item.has_text && item.has_image) return '文字 + 图片';
  if (item.has_image) return 'PNG 图片';
  return '文字';
}

export function clipboardHistoryRetentionLabel(settings: ClipboardHistorySettings): string {
  return settings.mode === 'count' ? '最近 ' + settings.count + ' 条' : '最近 ' + settings.days + ' 天';
}

export function platformLabel(platform: string | undefined): string {
  if (!platform) return '—';
  if (platform === 'windows') return 'Windows';
  if (platform === 'macos') return 'macOS';
  if (platform === 'linux') return 'Linux';
  if (platform === 'ios' || platform === 'android') return '手机';
  return platform;
}

export type ClientPlatform = 'windows' | 'macos' | 'linux' | 'other';

const CLIENT_PLATFORM_NAMES: Record<ClientPlatform, string> = {
  windows: 'Windows',
  macos: 'macOS',
  linux: 'Linux',
  other: '其他系统',
};

export function clientPlatform(userAgent: string): ClientPlatform {
  if (/Windows/.test(userAgent)) return 'windows';
  if (/Macintosh|Mac OS X/.test(userAgent)) return 'macos';
  if (/Linux/.test(userAgent)) return 'linux';
  return 'other';
}

export function clientPlatformName(platform: ClientPlatform): string {
  return CLIENT_PLATFORM_NAMES[platform];
}

export function webSessionLabel(userAgent: string): string {
  if (/iPhone|iPod/.test(userAgent)) return 'iPhone 浏览器';
  if (/iPad/.test(userAgent)) return 'iPad 浏览器';
  if (/Android/.test(userAgent)) return 'Android 浏览器';
  const platform = clientPlatform(userAgent);
  return platform === 'other' ? '网页设备' : clientPlatformName(platform) + ' 浏览器';
}

export function updateFrequencyLabel(value: UpdateFrequency): string {
  return value === 'daily' ? '每天' : value === 'weekly' ? '每周' : '关闭';
}

export function updateStatusLabel(value: UpdateStatus | null): string {
  if (!value) return '更新状态读取中';
  if (value.state === 'unavailable') return '开发构建不执行自动更新';
  if (value.state === 'disabled') return '自动更新已关闭';
  if (value.state === 'idle') return '等待下次检查';
  if (value.state === 'checking') return '正在检查 GitHub Release…';
  if (value.state === 'available') return '发现 v' + (value.available_version ?? '新版本');
  if (value.state === 'deferred') return '发现 v' + (value.available_version ?? '新版本') + '，等待传输完成';
  if (value.state === 'downloading') {
    if (!value.content_length) return '正在下载更新…';
    return '正在下载更新 ' + formatSize(value.downloaded_bytes) + ' / ' + formatSize(value.content_length);
  }
  if (value.state === 'installing') return value.error ?? '正在安装更新，应用即将重启';
  if (value.state === 'up_to_date') return '已是最新版本';
  return value.error ? '更新失败：' + value.error : '更新失败';
}

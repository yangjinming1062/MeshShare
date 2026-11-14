import type { ClipboardHistoryItem, ClipboardHistorySettings, HardwareMetrics, PeerInfo, UpdateFrequency, UpdateStatus } from './types';

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

export function metricAge(sampledAt: number | undefined, receivedAt?: number): string {
  const at = receivedAt ?? sampledAt;
  if (!at) return '等待数据';
  const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
  return seconds <= 1 ? '刚刚更新' : seconds + ' 秒前更新';
}

export function metricStatus(peer: PeerInfo | null, value: HardwareMetrics | null | undefined): string {
  if (peer?.metrics_stale) return '指标已过期';
  return metricAge(value?.sampled_at_ms, peer?.metrics_received_at_ms);
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

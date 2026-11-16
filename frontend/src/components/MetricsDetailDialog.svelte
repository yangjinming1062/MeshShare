<script lang="ts">
  import Dialog from './Dialog.svelte';
  import {
    diskUsedBytes,
    diskUsedPercent,
    formatDuration,
    formatFrequency,
    formatLoadAverage,
    formatMemory,
    formatPercent,
    formatRate,
    formatTime,
    memoryPercent,
  } from '../format';
  import type { DiskMetrics, GpuMemoryKind, GpuMetrics, PeerInfo } from '../types';

  export let open = false;
  export let peer: PeerInfo | null = null;
  export let onClose: () => void = () => {};

  const gpuMemoryKindLabels: Record<GpuMemoryKind, string> = {
    dedicated: '独立显存',
    unified: '统一内存',
    unknown: '类型未知',
  };

  $: stale = !!peer?.metrics_stale;
  $: displayMetrics = stale ? null : (peer?.metrics ?? null);
  $: gpus = displayMetrics?.gpus ?? [];
  $: disks = displayMetrics?.disks ?? [];
  $: hasSystemMetrics = !!displayMetrics && [
    displayMetrics.cpu_usage_percent,
    displayMetrics.cpu_model,
    displayMetrics.cpu_logical_cores,
    displayMetrics.cpu_physical_cores,
    displayMetrics.cpu_frequency_mhz,
    displayMetrics.memory_used_bytes,
    displayMetrics.memory_total_bytes,
    displayMetrics.memory_available_bytes,
    displayMetrics.swap_used_bytes,
    displayMetrics.swap_total_bytes,
    displayMetrics.load_average,
    displayMetrics.uptime_seconds,
  ].some((value) => value !== undefined && value !== null);

  function ratioPercent(used: number | undefined, total: number | undefined): number | undefined {
    if (used == null || total == null || total <= 0) return undefined;
    return used / total * 100;
  }

  function formatCores(logical: number | undefined, physical: number | undefined): string {
    if (logical != null && physical != null) return `${logical} 逻辑 / ${physical} 物理`;
    if (logical != null) return `${logical} 逻辑`;
    if (physical != null) return `${physical} 物理`;
    return '—';
  }

  function memoryKindLabel(kind: GpuMemoryKind): string {
    return gpuMemoryKindLabels[kind] ?? '类型未知';
  }

  function diskKey(disk: DiskMetrics, index: number): string {
    return `${disk.mount_point}:${disk.name}:${index}`;
  }

  function gpuKey(gpu: GpuMetrics, index: number): string {
    return `${gpu.name}:${index}`;
  }
</script>

<Dialog {open} title={(peer?.node_name ?? '设备') + ' 的硬件状态'} width={560} {onClose}>
  {#if stale}
    <div class="metrics-status metrics-status--warning">指标已过期，等待最新快照。</div>
  {:else if !displayMetrics}
    <div class="metrics-status">正在等待硬件指标。</div>
  {:else}
    <section class="metrics-section">
      <h3>系统</h3>
      {#if hasSystemMetrics}
        <div class="kv-row"><span class="muted">CPU 使用率</span><span>{formatPercent(displayMetrics.cpu_usage_percent)}</span></div>
        <div class="kv-row"><span class="muted">CPU 型号</span><span class="metrics-value">{displayMetrics.cpu_model ?? '—'}</span></div>
        <div class="kv-row"><span class="muted">CPU 核心</span><span>{formatCores(displayMetrics.cpu_logical_cores, displayMetrics.cpu_physical_cores)}</span></div>
        <div class="kv-row"><span class="muted">CPU 频率</span><span>{formatFrequency(displayMetrics.cpu_frequency_mhz)}</span></div>
        <div class="kv-row"><span class="muted">系统负载（1 / 5 / 15 分钟）</span><span>{formatLoadAverage(displayMetrics.load_average)}</span></div>
        <div class="kv-row"><span class="muted">运行时间</span><span>{formatDuration(displayMetrics.uptime_seconds)}</span></div>
      {:else}
        <div class="metrics-empty">此节点未提供系统硬件指标。</div>
      {/if}
    </section>

    <section class="metrics-section">
      <h3>内存</h3>
      <div class="kv-row"><span class="muted">已用 / 总量</span><span>{formatMemory(displayMetrics.memory_used_bytes)} / {formatMemory(displayMetrics.memory_total_bytes)}</span></div>
      <div class="kv-row"><span class="muted">使用率</span><span>{formatPercent(memoryPercent(displayMetrics))}</span></div>
      <div class="kv-row"><span class="muted">可用</span><span>{formatMemory(displayMetrics.memory_available_bytes)}</span></div>
      <div class="kv-row"><span class="muted">交换分区已用 / 总量</span><span>{formatMemory(displayMetrics.swap_used_bytes)} / {formatMemory(displayMetrics.swap_total_bytes)}</span></div>
      <div class="kv-row"><span class="muted">交换分区使用率</span><span>{formatPercent(ratioPercent(displayMetrics.swap_used_bytes, displayMetrics.swap_total_bytes))}</span></div>
    </section>

    <section class="metrics-section">
      <h3>网络</h3>
      <div class="kv-row"><span class="muted">下载</span><span>{formatRate(displayMetrics.network_down_bytes_per_sec)}</span></div>
      <div class="kv-row"><span class="muted">上传</span><span>{formatRate(displayMetrics.network_up_bytes_per_sec)}</span></div>
    </section>

    <section class="metrics-section">
      <h3>磁盘</h3>
      {#if disks.length}
        <div class="metrics-list">
          {#each disks as disk, index (diskKey(disk, index))}
            <div class="metrics-disk">
              <div class="metrics-disk-head">
                <strong class="metrics-value">{disk.name}</strong>
                {#if disk.removable}<span class="muted">可移动</span>{/if}
              </div>
              <div class="metrics-subvalue">{disk.mount_point}{#if disk.file_system} · {disk.file_system}{/if}</div>
              <div class="kv-row"><span class="muted">已用 / 总量</span><span>{formatMemory(diskUsedBytes(disk))} / {formatMemory(disk.total_bytes)}</span></div>
              <div class="kv-row"><span class="muted">可用 / 使用率</span><span>{formatMemory(disk.available_bytes)} / {formatPercent(diskUsedPercent(disk))}</span></div>
            </div>
          {/each}
        </div>
      {:else}
        <div class="metrics-empty">未检测到可读磁盘指标。</div>
      {/if}
    </section>

    <section class="metrics-section">
      <h3>GPU</h3>
      {#if gpus.length}
        <div class="metrics-list">
          {#each gpus as gpu, index (gpuKey(gpu, index))}
            <div class="metrics-gpu">
              <div class="metrics-gpu-head">
                <strong class="metrics-value">{gpu.name}</strong>
                <span>{memoryKindLabel(gpu.memory_kind)}</span>
              </div>
              <div class="kv-row"><span class="muted">利用率</span><span>{formatPercent(gpu.utilization_percent)}</span></div>
              <div class="kv-row"><span class="muted">显存已用 / 总量</span><span>{formatMemory(gpu.memory_used_bytes)} / {formatMemory(gpu.memory_total_bytes)}</span></div>
              {#if gpu.unavailable_reason}<div class="metrics-note">{gpu.unavailable_reason}</div>{/if}
            </div>
          {/each}
        </div>
      {:else}
        <div class="metrics-empty">GPU 指标暂不可用。</div>
      {/if}
    </section>

    <div class="metrics-sampled">采样时间：{formatTime(displayMetrics.sampled_at_ms) || '—'}</div>
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>关闭</button>
  </svelte:fragment>
</Dialog>

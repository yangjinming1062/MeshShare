<script lang="ts">
  import { formatMemory, formatPercent, formatRate, gpuUtilization, isMetricsStale, memoryPercent } from '../format';
  import type { HardwareMetrics } from '../types';

  export let metrics: HardwareMetrics | null = null;
  export let compactNetwork = false;
  export let monitorLocked = false;
  export let monitorVisible = true;
  export let canClickThrough = false;
  export let onToggleLock: () => void = () => {};
  export let onToggleVisible: () => void = () => {};

  $: stale = isMetricsStale(metrics);
  $: displayMetrics = stale ? null : metrics;
  $: gpus = displayMetrics?.gpus ?? [];
  $: shownGpus = gpus.slice(0, 2);
  $: moreGpus = Math.max(0, gpus.length - shownGpus.length);
  $: cpuPercent = displayMetrics?.cpu_usage_percent;
  $: memoryUsedPercent = memoryPercent(displayMetrics);
  $: gpuUsedPercent = gpuUtilization(displayMetrics);
  $: cpuBar = bar(cpuPercent);
  $: memoryBar = bar(memoryUsedPercent);
  $: gpuBar = bar(gpuUsedPercent);
  $: memoryCaption = stale
    ? '—'
    : formatMemory(metrics?.memory_used_bytes) + ' / ' + formatMemory(metrics?.memory_total_bytes);
  $: gpuCaption = stale ? '—' : (gpus.length ? gpus.length + ' 个 GPU' : '未检测到可读指标');

  function bar(value: number | undefined): number | undefined {
    if (value === undefined) return undefined;
    return Math.max(0, Math.min(1, value / 100));
  }
</script>

<section class="surface area-hardware">
  <div class="surface-head">
    <div class="surface-title">
      <h2>{compactNetwork ? '本机网络' : '本机硬件'}</h2>
    </div>
    <div class="surface-actions">
      <button
        type="button"
        class="btn btn-ghost btn-sm"
        aria-pressed={monitorVisible}
        title="显示或隐藏悬浮监控条"
        on:click={onToggleVisible}
      >{monitorVisible ? '隐藏悬浮条' : '显示悬浮条'}</button>
      <button
        type="button"
        class="btn btn-ghost btn-sm monitor-lock-toggle"
        aria-pressed={monitorLocked}
        disabled={!canClickThrough}
        title={canClickThrough ? '锁定或解锁悬浮监控条' : '当前桌面环境不支持点击穿透，可从托盘隐藏悬浮条'}
        on:click={onToggleLock}
      >{monitorLocked ? '解锁悬浮条' : '锁定悬浮条'}</button>
    </div>
  </div>
  <div class="surface-body">
    {#if compactNetwork}
      <div class="hw-grid">
        <div class="hw-tile">
          <span class="hw-label">上传</span>
          <strong class="hw-value">↑ {formatRate(displayMetrics?.network_up_bytes_per_sec)}</strong>
          <span class="hw-caption">本机网络实时速率</span>
        </div>
        <div class="hw-tile">
          <span class="hw-label">下载</span>
          <strong class="hw-value">↓ {formatRate(displayMetrics?.network_down_bytes_per_sec)}</strong>
          <span class="hw-caption">本机网络实时速率</span>
        </div>
      </div>
    {:else}
      <div class="hw-grid">
        <div class="hw-tile">
          <span class="hw-label">CPU</span>
          <strong class="hw-value">{formatPercent(cpuPercent)}</strong>
          {#if cpuBar !== undefined}<span class="hw-meter" aria-hidden="true"><span style="--p:{cpuBar}"></span></span>{/if}
          <span class="hw-caption">系统总使用率</span>
        </div>
        <div class="hw-tile">
          <span class="hw-label">内存</span>
          <strong class="hw-value">{formatPercent(memoryUsedPercent)}</strong>
          {#if memoryBar !== undefined}<span class="hw-meter" aria-hidden="true"><span style="--p:{memoryBar}"></span></span>{/if}
          <span class="hw-caption">{memoryCaption}</span>
        </div>
        <div class="hw-tile">
          <span class="hw-label">GPU</span>
          <strong class="hw-value">{formatPercent(gpuUsedPercent)}</strong>
          {#if gpuBar !== undefined}<span class="hw-meter" aria-hidden="true"><span style="--p:{gpuBar}"></span></span>{/if}
          <span class="hw-caption">{gpuCaption}</span>
        </div>
        <div class="hw-tile">
          <span class="hw-label">网络</span>
          <strong class="hw-value">↓ {formatRate(displayMetrics?.network_down_bytes_per_sec)}</strong>
          <span class="hw-caption">↑ {formatRate(displayMetrics?.network_up_bytes_per_sec)}</span>
          <span class="hw-caption">实时速率</span>
        </div>
      </div>
    {/if}
    {#if !compactNetwork && gpus.length}
      <div class="hw-gpus">
        {#each shownGpus as gpu, index (gpu.name + index)}
          <div class="hw-gpu" title={gpu.unavailable_reason}>
            <strong title={gpu.name}>{gpu.name}</strong>
            <span>{formatPercent(gpu.utilization_percent)}</span>
            <span>{formatMemory(gpu.memory_used_bytes)} / {formatMemory(gpu.memory_total_bytes)}</span>
          </div>
        {/each}
        {#if moreGpus}
          <span class="hw-more">还有 {moreGpus} 个 GPU</span>
        {/if}
      </div>
    {/if}
  </div>
</section>

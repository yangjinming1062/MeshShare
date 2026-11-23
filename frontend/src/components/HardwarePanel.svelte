<script lang="ts">
  import { formatMemory, formatPercent, formatRate, gpuUtilization, isMetricsStale, memoryPercent } from '../format';
  import type { HardwareMetrics } from '../types';

  export let remote = false;
  export let showMonitorControls = true;
  export let metrics: HardwareMetrics | null = null;
  export let monitorLocked = false;
  export let monitorVisible = true;
  export let canClickThrough = false;
  export let settingsDisabled = false;
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
  $: singleGpu = gpus.length === 1 ? gpus[0] : null;
  $: gpuCaption = stale
    ? '—'
    : singleGpu
      ? (singleGpu.memory_used_bytes != null && singleGpu.memory_total_bytes != null
        ? formatMemory(singleGpu.memory_used_bytes) + ' / ' + formatMemory(singleGpu.memory_total_bytes) + ' 显存'
        : '显存不可读')
      : gpus.length > 1
        ? gpus.length + ' 个 GPU'
        : '未检测到可读指标';

  function bar(value: number | undefined): number | undefined {
    if (value === undefined) return undefined;
    return Math.max(0, Math.min(1, value / 100));
  }
</script>

<section class="surface area-hardware">
  <div class="surface-head">
    <div class="surface-title">
      <h2>{remote ? '目标硬件' : '本机硬件'}</h2>
    </div>
    {#if showMonitorControls}
    <div class="surface-actions">
      <button
        type="button"
        class="btn btn-ghost btn-sm"
        aria-pressed={monitorVisible}
        disabled={settingsDisabled}
        title="显示或隐藏悬浮监控条"
        on:click={onToggleVisible}
      >{monitorVisible ? '隐藏悬浮条' : '显示悬浮条'}</button>
      <button
        type="button"
        class="btn btn-ghost btn-sm monitor-lock-toggle"
        aria-pressed={monitorLocked}
        disabled={!canClickThrough || settingsDisabled}
        title={canClickThrough ? '锁定或解锁悬浮监控条' : '当前桌面环境不支持点击穿透，可从托盘隐藏悬浮条'}
        on:click={onToggleLock}
      >{monitorLocked ? '取消点击穿透' : '开启点击穿透'}</button>
    </div>
    {/if}
  </div>
  <div class="surface-body">
    {#if stale}
      <p class="metrics-note">指标已过期，等待最新快照。</p>
    {/if}
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
    {#if gpus.length > 1}
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

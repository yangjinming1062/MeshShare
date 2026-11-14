<script lang="ts">
  import { formatMemory, formatPercent, formatRate, gpuUtilization, memoryPercent, metricStatus } from '../format';
  import type { HardwareMetrics } from '../types';

  export let metrics: HardwareMetrics | null = null;
  export let monitorLocked = false;
  export let canClickThrough = false;
  export let onToggleLock: () => void = () => {};

  $: gpus = metrics?.gpus ?? [];
  $: shownGpus = gpus.slice(0, 2);
  $: moreGpus = Math.max(0, gpus.length - shownGpus.length);
  $: status = metricStatus(null, metrics);
  $: cpuPercent = metrics?.cpu_usage_percent;
  $: memoryUsedPercent = memoryPercent(metrics);
  $: gpuUsedPercent = gpuUtilization(metrics);
  $: cpuBar = bar(cpuPercent);
  $: memoryBar = bar(memoryUsedPercent);
  $: gpuBar = bar(gpuUsedPercent);

  function bar(value: number | undefined): number | undefined {
    if (value === undefined) return undefined;
    return Math.max(0, Math.min(1, value / 100));
  }
</script>

<section class="surface area-hardware">
  <div class="surface-head">
    <div class="surface-title">
      <h2>本机硬件</h2>
      <span class="muted">{status}</span>
    </div>
    <div class="surface-actions">
      <button
        type="button"
        class="btn btn-ghost btn-sm monitor-lock-toggle"
        aria-pressed={monitorLocked}
        disabled={!canClickThrough}
        title={canClickThrough ? '锁定或解锁悬浮监控条' : 'Wayland 下不支持点击穿透，置顶由合成器管理'}
        on:click={onToggleLock}
      >{monitorLocked ? '解锁悬浮条' : '锁定悬浮条'}</button>
    </div>
  </div>
  <div class="surface-body">
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
        <span class="hw-caption">{formatMemory(metrics?.memory_used_bytes)} / {formatMemory(metrics?.memory_total_bytes)}</span>
      </div>
      <div class="hw-tile">
        <span class="hw-label">GPU</span>
        <strong class="hw-value">{formatPercent(gpuUsedPercent)}</strong>
        {#if gpuBar !== undefined}<span class="hw-meter" aria-hidden="true"><span style="--p:{gpuBar}"></span></span>{/if}
        <span class="hw-caption">{gpus.length ? gpus.length + ' 个 GPU' : '未检测到可读指标'}</span>
      </div>
      <div class="hw-tile">
        <span class="hw-label">网络</span>
        <strong class="hw-value">↓ {formatRate(metrics?.network_down_bytes_per_sec)}</strong>
        <span class="hw-caption">↑ {formatRate(metrics?.network_up_bytes_per_sec)}</span>
        <span class="hw-caption">实时速率</span>
      </div>
    </div>
    {#if gpus.length}
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

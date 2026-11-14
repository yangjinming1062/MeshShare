<script lang="ts">
  import Dialog from './Dialog.svelte';
  import { formatMemory, formatPercent, formatRate, metricStatus } from '../format';
  import type { PeerInfo } from '../types';

  export let open = false;
  export let peer: PeerInfo | null = null;
  export let onClose: () => void = () => {};

  $: gpus = peer?.metrics?.gpus ?? [];
</script>

<Dialog {open} title={(peer?.node_name ?? '设备') + ' 的硬件状态'} width={440} {onClose}>
  <div class="kv-row">
    <span class="muted">更新时间</span>
    <span>{metricStatus(peer, peer?.metrics ?? null)}</span>
  </div>
  <div class="kv-row">
    <span class="muted">CPU</span>
    <span>{formatPercent(peer?.metrics?.cpu_usage_percent)}</span>
  </div>
  <div class="kv-row">
    <span class="muted">网络</span>
    <span>↓ {formatRate(peer?.metrics?.network_down_bytes_per_sec)} · ↑ {formatRate(peer?.metrics?.network_up_bytes_per_sec)}</span>
  </div>
  {#if gpus.length}
    <div class="hw-gpus">
      {#each gpus as gpu, index (gpu.name + index)}
        <div class="hw-gpu" title={gpu.unavailable_reason}>
          <strong title={gpu.name}>{gpu.name}</strong>
          <span>{formatPercent(gpu.utilization_percent)}</span>
          <span>{formatMemory(gpu.memory_used_bytes)} / {formatMemory(gpu.memory_total_bytes)}</span>
        </div>
        {#if gpu.unavailable_reason}
          <span class="hw-note">{gpu.unavailable_reason}</span>
        {/if}
      {/each}
    </div>
  {:else}
    <div class="empty">GPU/显存指标暂不可用，CPU、内存和网络仍会继续采集。</div>
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>关闭</button>
  </svelte:fragment>
</Dialog>

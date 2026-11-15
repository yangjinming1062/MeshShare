<script lang="ts">
  import Icon from './Icon.svelte';
  import Meter from './Meter.svelte';
  import { formatConnectedSince, formatPercent, formatRateCompact, gpuUtilization, memoryPercent, peerSummary } from '../format';
  import { platformLabel } from '../format';
  import type { MobileSession, PeerInfo } from '../types';

  export let kind: 'peer' | 'mobile' = 'peer';
  export let peer: PeerInfo | null = null;
  export let session: MobileSession | null = null;
  export let dragging = false;
  export let onSend: () => void = () => {};
  export let onSendFolder: () => void = () => {};
  export let onDetails: () => void = () => {};
  export let onDragOver: () => void = () => {};
  export let onDragLeave: () => void = () => {};
  export let onDropFile: (event: DragEvent) => void = () => {};

  $: name = kind === 'mobile' ? (session?.label ?? '网页设备') : (peer?.node_name ?? '设备');
  $: meta = kind === 'mobile'
    ? (session ? session.node_name + ' · 网页加入' : '网页加入')
    : (peer ? platformLabel(peer.platform) + ' · ' + (peer.addresses[0]?.address ?? '在线') : '');
  $: metrics = kind === 'mobile' ? null : (peer?.metrics ?? null);
  $: stale = kind === 'peer' && !!peer?.metrics_stale;
  $: hasMetrics = !!metrics;
  $: displayMetrics = stale ? null : metrics;
  $: cpu = displayMetrics?.cpu_usage_percent;
  $: memory = memoryPercent(displayMetrics);
  $: gpu = gpuUtilization(displayMetrics);

  function isInsideRow(event: DragEvent): boolean {
    const row = event.currentTarget;
    const relatedTarget = event.relatedTarget;
    return row instanceof Node && relatedTarget instanceof Node && row.contains(relatedTarget);
  }

  function handleDragEnter(event: DragEvent) {
    if (!isInsideRow(event)) onDragOver();
  }

  function handleDragLeave(event: DragEvent) {
    if (!isInsideRow(event)) onDragLeave();
  }
</script>

<li
  class="device-row"
  class:dragging
  on:dragenter|preventDefault={handleDragEnter}
  on:dragover|preventDefault={() => onDragOver()}
  on:dragleave={handleDragLeave}
  on:drop|preventDefault={(event) => onDropFile(event)}
>
  <span class="device-icon" aria-hidden="true">
    <Icon name={kind === 'mobile' ? 'phone' : 'monitor'} size={18} />
  </span>
  <div class="device-id">
    <strong title={name}>{name}</strong>
    <span class="device-meta" title={meta}>{meta}</span>
  </div>
  {#if kind === 'mobile'}
    <div class="device-metrics device-metrics--mobile">
      <span class="muted">已连接 {formatConnectedSince(session?.created_at_ms ?? Date.now())}</span>
    </div>
  {:else}
    <button
      type="button"
      class="device-metrics"
      disabled={!hasMetrics}
      aria-label={'查看 ' + name + ' 的硬件详情'}
      title={hasMetrics && peer ? peerSummary(peer) : (stale ? '指标已过期' : '指标采集中')}
      on:click={onDetails}
    >
      {#if hasMetrics}
        <Meter label="CPU" value={formatPercent(cpu)} percent={cpu} />
        <Meter label="内存" value={formatPercent(memory)} percent={memory} />
        <Meter label="GPU" value={formatPercent(gpu)} percent={gpu} />
        <span class="meter meter-net">
          <b class="meter-value">↓ {formatRateCompact(displayMetrics?.network_down_bytes_per_sec)}</b>
          <b class="meter-value">↑ {formatRateCompact(displayMetrics?.network_up_bytes_per_sec)}</b>
        </span>
      {:else}
        <span class="device-metrics-empty">指标采集中</span>
      {/if}
    </button>
  {/if}
  <div class="device-actions">
    <button type="button" class="btn btn-primary btn-sm" on:click={onSend}>
      <Icon name="send" size={13} />
      发送
    </button>
    <button type="button" class="btn-icon" aria-label={'向 ' + name + ' 发送文件夹'} title="发送文件夹" on:click={onSendFolder}>
      <Icon name="folder" size={16} />
    </button>
  </div>
  {#if dragging}
    <div class="drop-hint" aria-hidden="true">松开即可发送</div>
  {/if}
</li>

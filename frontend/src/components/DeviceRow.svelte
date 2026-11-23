<script lang="ts">
  import DeviceIdentity from './DeviceIdentity.svelte';
  import Icon from './Icon.svelte';
  import Meter from './Meter.svelte';
  import { formatConnectedSince, formatPercent, formatRate, formatRateCompact, gpuMemoryPercent, gpuUtilization, memoryPercent } from '../format';
  import type { MobileSession, PeerInfo } from '../types';

  export let kind: 'peer' | 'mobile' = 'peer';
  export let peer: PeerInfo | null = null;
  export let session: MobileSession | null = null;
  export let dragging = false;
  export let linkActive = false;
  export let isHome = false;
  export let onSend: () => void = () => {};
  export let onSendFolder: () => void = () => {};
  export let onDetails: () => void = () => {};
  export let onTerminal: (() => void) | null = null;
  export let onOpen: () => void = () => {};
  export let allowFolderSend = true;
  export let onDragOver: () => void = () => {};
  export let onDragLeave: () => void = () => {};
  export let onDropFile: (event: DragEvent) => void = () => {};

  $: name = kind === 'mobile' ? (session?.label ?? '网页设备') : (peer?.node_name ?? '设备');
  $: browserSource = kind === 'mobile' ? (session?.node_name ?? '') : undefined;
  $: peerKey = peer ? encodeURIComponent(peer.node_id) : undefined;
  $: identityDescriptionId = peerKey ? 'device-meta-' + peerKey : undefined;
  $: metricsDescriptionId = peerKey ? 'device-metrics-' + peerKey : undefined;
  $: metrics = kind === 'mobile' ? null : (peer?.metrics ?? null);
  $: stale = kind === 'peer' && !!peer?.metrics_stale;
  $: displayMetrics = stale ? null : metrics;
  $: cpu = displayMetrics?.cpu_usage_percent;
  $: gpu = gpuUtilization(displayMetrics);
  $: upload = displayMetrics?.network_up_bytes_per_sec;
  $: memory = memoryPercent(displayMetrics);
  $: vram = gpuMemoryPercent(displayMetrics);
  $: download = displayMetrics?.network_down_bytes_per_sec;
  $: metricsSummary = displayMetrics
    ? [
        `CPU ${formatPercent(cpu)}`, `GPU ${formatPercent(gpu)}`, `上传 ${formatRate(upload)}`,
        `内存 ${formatPercent(memory)}`, `显存 ${formatPercent(vram)}`, `下载 ${formatRate(download)}`,
      ].join(' · ')
    : stale ? '指标已过期' : '指标采集中';
  $: openable = kind === 'peer' && (isHome || !!peer?.addresses.length);

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
  data-link={linkActive ? 'active' : undefined}
  on:dragenter|preventDefault={handleDragEnter}
  on:dragover|preventDefault={() => onDragOver()}
  on:dragleave={handleDragLeave}
  on:drop|preventDefault={(event) => onDropFile(event)}
>
  <button
    type="button"
    class="device-identity"
    class:device-identity--openable={openable}
    disabled={!openable}
    aria-label={isHome ? '返回本机' : openable ? '管理 ' + name : name}
    aria-describedby={identityDescriptionId}
    title={isHome ? '返回本机' : openable ? '进入 ' + name : name}
    on:click|stopPropagation={onOpen}
  >
    <DeviceIdentity {name} {peer} {browserSource} {isHome} descriptionId={identityDescriptionId} />
  </button>
  {#if kind === 'mobile'}
    <div class="device-metrics device-metrics--mobile">
      <span class="muted">已连接 {formatConnectedSince(session?.created_at_ms ?? Date.now())}</span>
    </div>
  {:else}
    <button
      type="button"
      class="device-metrics"
      disabled={!metrics}
      aria-label={'查看 ' + name + ' 的硬件详情'}
      aria-describedby={metricsDescriptionId}
      title={metricsSummary}
      on:click={onDetails}
    >
      {#if displayMetrics}
        <Meter label="CPU" value={formatPercent(cpu)} percent={cpu} />
        <Meter label="GPU" value={formatPercent(gpu)} percent={gpu} />
        <Meter label="↑" value={formatRateCompact(upload)} />
        <Meter label="内存" value={formatPercent(memory)} percent={memory} />
        <Meter label="显存" value={formatPercent(vram)} percent={vram} />
        <Meter label="↓" value={formatRateCompact(download)} />
        <span id={metricsDescriptionId} hidden>{metricsSummary}</span>
      {:else}
        <span id={metricsDescriptionId} class="device-metrics-empty" data-stale={stale}>{metricsSummary}</span>
      {/if}
    </button>
  {/if}
  <div class="device-actions" class:device-actions--without-terminal={!onTerminal}>
    {#if onTerminal}
      <button type="button" class="btn-icon" aria-label={'打开 ' + name + ' 的终端'} title={'打开 ' + name + ' 的终端'} on:click={onTerminal}><Icon name="terminal" size={16} /></button>
    {/if}
    <button type="button" class="btn btn-send btn-sm" title="发送文件" on:click={onSend}>
      <Icon name="send" size={13} />
      发送文件
    </button>
    {#if allowFolderSend}
      <button type="button" class="btn-icon" aria-label={'向 ' + name + ' 发送文件夹'} title="发送文件夹" on:click={onSendFolder}>
        <Icon name="folder" size={16} />
      </button>
    {/if}
  </div>
  {#if dragging}
    <div class="drop-hint" aria-hidden="true">松开即可发送</div>
  {/if}
</li>

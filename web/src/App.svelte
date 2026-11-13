<script lang="ts">
  import { onMount } from 'svelte';
  import QRCode from 'qrcode';
  import { open } from '@tauri-apps/plugin-dialog';
  import { invoke } from '@tauri-apps/api/core';
  import { getClipboard, getInfo, getMetrics, getPeers, initApi, request, websocketUrl } from './api';
  import type { ClipboardSnapshot, HardwareMetrics, NodeInfo, PeerInfo, TransferItem, TransferOffer, TransferProgress } from './types';

  const isMobile = window.location.pathname.startsWith('/mobile');
  const isMonitor = new URLSearchParams(window.location.search).get('window') === 'monitor';
  let node: NodeInfo | null = null;
  let peers: PeerInfo[] = [];
  let clipboard: ClipboardSnapshot | null = null;
  let metrics: HardwareMetrics | null = null;
  let selectedMetricsPeer: PeerInfo | null = null;
  $: currentMetrics = selectedMetricsPeer?.metrics ?? metrics;
  let clipboardDraft = '';
  let status = '正在连接…';
  let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  let selectedAddress = '';
  let qrData = '';
  let syncEnabled = true;
  let ws: WebSocket | null = null;
  let fileInput: HTMLInputElement;
  let folderInput: HTMLInputElement;
  let selectedPeer: PeerInfo | null = null;
  let dragPeer = '';
  let notice = '';
  let transfers: TransferProgress[] = [];
  let manualAddress = '';
  const pendingUploads = new Map<string, { target: string; files: File[]; items: TransferItem[] }>();
  const uploading = new Set<string>();

  $: selectedUrl = selectedAddress || node?.addresses[0]?.url || '';
  $: mobileUrl = selectedUrl ? `${selectedUrl}/mobile` : '';

  onMount(() => {
    folderInput?.setAttribute('webkitdirectory', '');
    folderInput?.setAttribute('directory', '');
    let timer: number | undefined;
    let stopped = false;
    const start = async () => {
      try {
        await initApi();
        await refresh();
        syncEnabled = await invoke<boolean>('clipboard_sync_enabled').catch(() => true);
        connectWebSocket();
        timer = window.setInterval(refresh, 2500);
      } catch (error) {
        status = error instanceof Error ? error.message : '无法连接节点';
        statusKind = 'error';
        timer = window.setTimeout(start, 3000);
      }
    };
    void start();
    return () => {
      stopped = true;
      if (timer) window.clearInterval(timer);
      ws?.close();
      if (stopped) window.clearTimeout(timer);
    };
  });

  async function refresh() {
    const [nextNode, nextPeers, nextClipboard, nextMetrics] = await Promise.all([getInfo(), getPeers(), getClipboard(), getMetrics()]);
    node = nextNode;
    peers = nextPeers.filter((peer) => peer.online);
    clipboard = nextClipboard;
    metrics = nextMetrics;
    if (selectedMetricsPeer) {
      selectedMetricsPeer = peers.find((peer) => peer.node_id === selectedMetricsPeer?.node_id) ?? null;
    }
    if (clipboard && document.activeElement?.id !== 'clipboard-editor') clipboardDraft = clipboard.text ?? '';
    if (!selectedAddress || !nextNode.addresses.some((address) => address.url === selectedAddress)) selectedAddress = nextNode.addresses[0]?.url ?? '';
    status = '节点在线';
    statusKind = 'ok';
    if (!isMonitor) await refreshQr();
  }

  function connectWebSocket() {
    ws?.close();
    ws = new WebSocket(websocketUrl());
    ws.onopen = () => { status = '已连接'; statusKind = 'ok'; };
    ws.onclose = () => { status = '连接断开，重试中…'; statusKind = 'warn'; window.setTimeout(connectWebSocket, 1800); };
    ws.onerror = () => { status = '同步连接失败'; statusKind = 'error'; };
    ws.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data) as { type: string; payload: unknown };
        handleWireMessage(message.type, message.payload);
      } catch {
        // A malformed event must not break the live UI.
      }
    };
  }

  function handleWireMessage(type: string, payload: unknown) {
    if (type === 'Hello') {
      void refresh();
      return;
    }
    if (type === 'ClipboardUpdate') {
      void refresh();
      return;
    }
    if (type === 'MetricsUpdate') {
      const nextMetrics = payload as HardwareMetrics;
      if (nextMetrics.node_id === node?.node_id) {
        metrics = nextMetrics;
      } else {
        peers = peers.map((peer) => peer.node_id === nextMetrics.node_id
          ? { ...peer, metrics: nextMetrics, metrics_received_at_ms: Date.now(), metrics_stale: false }
          : peer);
        if (selectedMetricsPeer?.node_id === nextMetrics.node_id) {
          selectedMetricsPeer = { ...selectedMetricsPeer, metrics: nextMetrics, metrics_received_at_ms: Date.now(), metrics_stale: false };
        }
      }
      return;
    }
    if (type === 'TransferOffer') {
      const offer = payload as TransferOffer;
      transfers = [{
        transfer_id: offer.transfer_id,
        status: 'waiting',
        completed_bytes: 0,
        total_bytes: offer.total_bytes,
        completed_items: 0,
        total_items: offer.items.length,
        offer,
        direction: 'receive',
      }, ...transfers.filter((item) => item.transfer_id !== offer.transfer_id)];
      return;
    }
    if (type === 'TransferProgress') {
      const progress = payload as TransferProgress;
      const existing = transfers.find((item) => item.transfer_id === progress.transfer_id);
      transfers = [{ ...existing, ...progress }, ...transfers.filter((item) => item.transfer_id !== progress.transfer_id)];
    }
  }

  async function refreshQr() {
    if (!mobileUrl) {
      qrData = '';
      return;
    }
    qrData = await QRCode.toDataURL(mobileUrl, { width: 192, margin: 1, color: { dark: '#111827', light: '#ffffff' } });
  }

  async function toggleSync() {
    syncEnabled = await invoke<boolean>('toggle_clipboard_sync').catch(() => !syncEnabled);
    notice = syncEnabled ? '剪贴板自动同步已恢复' : '剪贴板自动同步已暂停';
    window.setTimeout(() => (notice = ''), 2200);
  }

  function selectMetricsPeer(peer: PeerInfo) {
    selectedMetricsPeer = peer;
  }

  async function hideMonitor() {
    await invoke('set_monitor_display_mode', { mode: 'tray' }).catch(() => undefined);
  }

  async function publishClipboard() {
    if (!clipboardDraft.trim()) return;
    try {
      clipboard = await request<ClipboardSnapshot>('/api/v1/clipboard', {
        method: 'POST',
        body: JSON.stringify({ text: clipboardDraft, source_name: node?.node_name }),
      });
      notice = '已同步到在线设备';
    } catch (error) {
      notice = error instanceof Error ? error.message : '剪贴板同步失败';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function connectManualPeer() {
    const url = manualAddress.trim().replace(/\/$/, '');
    if (!url) return;
    try {
      await request('/api/v1/peers/connect', { method: 'POST', body: JSON.stringify({ url }) });
      manualAddress = '';
      await refresh();
      notice = '已发起节点连接';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法连接节点';
    }
    window.setTimeout(() => (notice = ''), 2400);
  }

  async function readMobileClipboard() {
    try {
      clipboardDraft = await navigator.clipboard.readText();
      notice = '已读取手机剪贴板';
    } catch {
      notice = '浏览器未授权读取剪贴板，请手动粘贴';
    }
  }

  async function writeMobileClipboard() {
    try {
      await navigator.clipboard.writeText(clipboardDraft);
      notice = '已写入手机剪贴板';
    } catch {
      notice = '浏览器拒绝写入，请手动选择并复制';
    }
  }

  function startChoose(peer: PeerInfo, folder = false) {
    selectedPeer = peer;
    if (folder) folderInput?.click();
    else fileInput?.click();
  }

  async function onFileInput(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    if (selectedPeer && input.files) await sendFiles(selectedPeer, Array.from(input.files));
    input.value = '';
  }

  async function onDrop(peer: PeerInfo, event: DragEvent) {
    event.preventDefault();
    dragPeer = '';
    const files = event.dataTransfer ? await filesFromDataTransfer(event.dataTransfer) : [];
    if (files.length) await sendFiles(peer, files);
  }

  type DropEntry = FileSystemEntry & { isFile: boolean; isDirectory: boolean; file?: (callback: (file: File) => void) => void; createReader?: () => { readEntries: (callback: (entries: DropEntry[]) => void) => void } };

  async function filesFromDataTransfer(data: DataTransfer): Promise<File[]> {
    const files: File[] = [];
    const items = Array.from(data.items);
    for (const item of items) {
      const entry = (item as DataTransferItem & { webkitGetAsEntry?: () => DropEntry | null }).webkitGetAsEntry?.();
      if (entry) await readDropEntry(entry, '', files);
    }
    return files.length ? files : Array.from(data.files);
  }

  async function readDropEntry(entry: DropEntry, prefix: string, files: File[]): Promise<void> {
    if (entry.isFile && entry.file) {
      const file = await new Promise<File>((resolve) => entry.file!(resolve));
      const relativePath = `${prefix}${file.name}`;
      Object.defineProperty(file, 'webkitRelativePath', { value: relativePath, configurable: true });
      files.push(file);
      return;
    }
    if (!entry.isDirectory || !entry.createReader) return;
    const reader = entry.createReader();
    const entries: DropEntry[] = [];
    while (true) {
      const batch = await new Promise<DropEntry[]>((resolve) => reader.readEntries(resolve));
      if (!batch.length) break;
      entries.push(...batch);
    }
    for (const child of entries) await readDropEntry(child, `${prefix}${entry.name}/`, files);
  }

  function targetBase(peer: PeerInfo): string | null {
    const url = peer.addresses.find((address) => address.kind === 'lan')?.url ?? peer.addresses[0]?.url;
    return url ?? (peer.endpoint ? peer.endpoint.replace(/\/api\/v1\/ws$/, '') : null);
  }

  async function createItem(file: File, index: number): Promise<TransferItem> {
    const relativePath = (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
    const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
    const sha256 = Array.from(new Uint8Array(digest)).map((value) => value.toString(16).padStart(2, '0')).join('');
    return {
      item_id: `${index}-${crypto.randomUUID()}`,
      name: file.name,
      relative_path: relativePath,
      size: file.size,
      sha256,
      media_type: file.type || 'application/octet-stream',
      directory: false,
    };
  }

  async function sendFiles(peer: PeerInfo, files: File[]) {
    const target = targetBase(peer);
    if (!target || !node) {
      notice = '目标设备没有可访问地址';
      return;
    }
    selectedPeer = null;
    notice = `正在准备发送 ${files.length} 个项目…`;
    try {
      const items = await Promise.all(files.map(createItem));
      const offer: TransferOffer = {
        transfer_id: crypto.randomUUID(),
        source_node_id: node.node_id,
        source_name: node.node_name,
        target_node_id: peer.node_id,
        items,
        total_bytes: items.reduce((sum, item) => sum + item.size, 0),
        created_at_ms: Date.now(),
      };
      await request<TransferProgress>('/api/v1/transfers', { method: 'POST', body: JSON.stringify(offer) }, target);
      pendingUploads.set(offer.transfer_id, { target, files, items });
      transfers = [{ transfer_id: offer.transfer_id, status: 'waiting', completed_bytes: 0, total_bytes: offer.total_bytes, completed_items: 0, total_items: items.length, offer, direction: 'send' }, ...transfers];
      void watchTransfer(offer.transfer_id, target);
      notice = '已发送传输请求，等待接收端确认';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法创建传输';
    }
  }

  async function watchTransfer(id: string, target: string) {
    for (let attempt = 0; attempt < 180; attempt += 1) {
      try {
        const progress = await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(id)}/status`, {}, target);
        transfers = [{ ...transfers.find((item) => item.transfer_id === id), ...progress }, ...transfers.filter((item) => item.transfer_id !== id)];
        if (progress.status === 'accepted' && !uploading.has(id)) await uploadTransfer(id, target);
        if (['completed', 'rejected', 'failed', 'cancelled'].includes(progress.status)) {
          pendingUploads.delete(id);
          return;
        }
      } catch {
        // The target may be briefly offline; the next poll will recover it.
      }
      await new Promise((resolve) => window.setTimeout(resolve, 800));
    }
  }

  async function uploadTransfer(id: string, target: string) {
    const pending = pendingUploads.get(id);
    if (!pending) return;
    uploading.add(id);
    try {
      for (const item of pending.items) {
        const file = pending.files.find((candidate) => ((candidate as File & { webkitRelativePath?: string }).webkitRelativePath || candidate.name) === item.relative_path) ?? pending.files[pending.items.indexOf(item)];
        const response = await fetch(`${target}/api/v1/transfers/${encodeURIComponent(id)}/items/${encodeURIComponent(item.item_id)}`, {
          method: 'PUT',
          headers: { 'Content-Type': item.media_type },
          body: file,
        });
        if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error ?? `上传失败 (${response.status})`);
      }
      notice = '文件发送完成';
    } catch (error) {
      notice = error instanceof Error ? error.message : '文件发送失败';
      await request(`/api/v1/transfers/${encodeURIComponent(id)}/cancel`, { method: 'POST' }, target).catch(() => undefined);
    } finally {
      uploading.delete(id);
    }
  }

  async function acceptTransfer(transfer: TransferProgress) {
    if (!transfer.offer) return;
    let destination: string | string[] | null = null;
    try {
      destination = await open({ directory: true, multiple: false, title: '选择接收目录' });
    } catch {
      destination = null;
    }
    const path = Array.isArray(destination) ? destination[0] : destination;
    if (!path) {
      notice = '当前环境无法选择接收目录';
      return;
    }
    try {
      await request<TransferProgress>(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/accept`, { method: 'POST', body: JSON.stringify({ destination: path }) });
      notice = '已接受，等待文件直传';
    } catch (error) {
      notice = error instanceof Error ? error.message : '无法接受传输';
    }
  }

  async function rejectTransfer(transfer: TransferProgress) {
    await request(`/api/v1/transfers/${encodeURIComponent(transfer.transfer_id)}/reject`, { method: 'POST' }).catch(() => undefined);
  }

  function formatSize(bytes: number) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
    return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
  }

  function formatRate(value: number | undefined) {
    if (value === undefined) return '—';
    if (value >= 1024 ** 2) return `${(value / 1024 ** 2).toFixed(1)} MB/s`;
    if (value >= 1024) return `${(value / 1024).toFixed(1)} KB/s`;
    return `${Math.round(value)} B/s`;
  }

  function formatPercent(value: number | undefined) {
    return value === undefined ? '—' : `${Math.round(value)}%`;
  }

  function formatMemory(value: number | undefined) {
    return value === undefined ? '—' : formatSize(value);
  }

  function memoryPercent(value: HardwareMetrics | null | undefined) {
    if (value?.memory_used_bytes === undefined || value.memory_total_bytes === undefined) return undefined;
    if (value.memory_total_bytes <= 0) return undefined;
    return value.memory_used_bytes / value.memory_total_bytes * 100;
  }

  function gpuSummary(value: HardwareMetrics | null | undefined) {
    const gpus = value?.gpus ?? [];
    const utilization = gpus.map((gpu) => gpu.utilization_percent).filter((item): item is number => item !== undefined);
    const used = gpus.map((gpu) => gpu.memory_used_bytes).filter((item): item is number => item !== undefined).reduce((sum, item) => sum + item, 0);
    const total = gpus.map((gpu) => gpu.memory_total_bytes).filter((item): item is number => item !== undefined).reduce((sum, item) => sum + item, 0);
    const usage = utilization.length ? formatPercent(Math.max(...utilization)) : '—';
    const memory = total ? `${formatSize(used)} / ${formatSize(total)}` : '—';
    return `${usage} · ${memory}`;
  }

  function metricAge(sampledAt: number | undefined, receivedAt?: number) {
    const at = receivedAt ?? sampledAt;
    if (!at) return '等待数据';
    const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
    return seconds <= 1 ? '刚刚更新' : `${seconds} 秒前更新`;
  }

  function metricStatus(peer: PeerInfo | null, value: HardwareMetrics | null) {
    if (peer?.metrics_stale) return '指标已过期';
    return metricAge(value?.sampled_at_ms, peer?.metrics_received_at_ms);
  }

  function peerSummary(peer: PeerInfo) {
    if (peer.metrics_stale) return '指标已过期';
    const value = peer.metrics;
    if (!value) return '指标采集中';
    return `CPU ${formatPercent(value.cpu_usage_percent)} · 内存 ${formatPercent(memoryPercent(value))} · ↓ ${formatRate(value.network_down_bytes_per_sec)} ↑ ${formatRate(value.network_up_bytes_per_sec)}`;
  }

  function formatTime(value: number | undefined) {
    return value ? new Date(value).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
  }
</script>

<svelte:head>
  <title>{isMonitor ? 'MeshShare · 硬件监控' : isMobile ? 'MeshShare · 手机连接' : 'MeshShare · 局域网共享'}</title>
</svelte:head>

<div class:mobile-shell={isMobile} class:monitor-shell={isMonitor} class="shell">
  {#if isMonitor}
    <main class="monitor-main" data-tauri-drag-region>
      <div class="monitor-brand" data-tauri-drag-region><span class="monitor-dot"></span><strong>MeshShare</strong></div>
      <div class="monitor-metric"><b>CPU</b><span>{formatPercent(metrics?.cpu_usage_percent)}</span></div>
      <div class="monitor-metric"><b>GPU</b><span>{gpuSummary(metrics).split(' · ')[0]}</span></div>
      <div class="monitor-metric"><b>显存</b><span>{gpuSummary(metrics).split(' · ')[1] ?? '—'}</span></div>
      <div class="monitor-metric"><b>内存</b><span>{formatPercent(memoryPercent(metrics))}</span></div>
      <div class="monitor-metric"><b>↓</b><span>{formatRate(metrics?.network_down_bytes_per_sec)}</span></div>
      <div class="monitor-metric"><b>↑</b><span>{formatRate(metrics?.network_up_bytes_per_sec)}</span></div>
      <button class="monitor-close" aria-label="隐藏监控条" on:click={hideMonitor}>×</button>
    </main>
  {:else}
  <header class="topbar">
    <div class="brand-block">
      <img class="brand-mark" src="/meshshare.svg" width="40" height="40" alt="" />
      <div>
        <div class="brand-name">MeshShare</div>
        <div class="brand-subtitle">{isMobile ? '扫码加入局域网' : '设备之间，直接共享'}</div>
      </div>
    </div>
    <div class="status-pill" data-kind={statusKind}><span class="status-dot"></span>{status}</div>
  </header>

  {#if notice}<div class="notice" role="status">{notice}</div>{/if}

  {#if isMobile}
    <main class="mobile-main">
      <section class="mobile-connection panel">
        <div class="section-kicker">当前连接</div>
        <h1>{node?.node_name ?? 'MeshShare 节点'}</h1>
        <p>选择下面的设备，直接发送文件或文件夹。</p>
      </section>
      <section class="panel clipboard-panel">
        <div class="panel-heading"><div><div class="section-kicker">CLIPBOARD</div><h2>共享剪贴板</h2></div><span class="muted">{clipboard ? formatTime(clipboard.updated_at_ms) : '暂无内容'}</span></div>
        <textarea id="clipboard-editor" bind:value={clipboardDraft} placeholder="粘贴或输入需要同步的文字"></textarea>
        <div class="button-row"><button class="primary" on:click={publishClipboard}>同步到设备</button><button on:click={readMobileClipboard}>读取手机剪贴板</button><button on:click={writeMobileClipboard}>写入手机剪贴板</button></div>
      </section>
      <section class="panel">
        <div class="panel-heading"><div><div class="section-kicker">DEVICES</div><h2>在线设备</h2></div><span class="count-badge">{peers.length}</span></div>
        <div class="peer-grid mobile-peers">
          {#each peers as peer}
            <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
            <article role="button" tabindex="0" class="peer-card" on:click={() => startChoose(peer)} on:keydown={(event) => event.key === 'Enter' && startChoose(peer)} on:dragover|preventDefault on:drop={(event) => onDrop(peer, event)}>
              <div class="peer-icon">{peer.node_name.slice(0, 1).toUpperCase()}</div>
              <div class="peer-copy"><strong>{peer.node_name}</strong><span>{peer.platform} · 在线</span></div>
              <div class="peer-actions"><button on:click|stopPropagation={() => startChoose(peer)}>发送文件</button><button class="ghost" on:click|stopPropagation={() => startChoose(peer, true)}>发送文件夹</button></div>
            </article>
          {:else}<div class="empty-state">暂时没有发现其他在线桌面设备。</div>{/each}
        </div>
      </section>
    </main>
  {:else}
    <main class="desktop-main">
      <section class="hero-grid">
        <div class="panel hero-panel">
          <div class="section-kicker">LOCAL NODE</div>
          <div class="hero-title-row"><div><h1>{node?.node_name ?? 'MeshShare 节点'}</h1><p>当前设备正在局域网中提供直接连接。</p></div><button class:active={syncEnabled} class="sync-toggle" on:click={toggleSync}><span></span>{syncEnabled ? '自动同步中' : '同步已暂停'}</button></div>
          <div class="hero-stats"><div><b>{peers.length}</b><span>在线设备</span></div><div><b>{node?.platform ?? '—'}</b><span>运行平台</span></div><div><b>{node?.port ?? '—'}</b><span>服务端口</span></div></div>
        </div>
        <div class="panel access-panel">
          <div class="panel-heading"><div><div class="section-kicker">MOBILE ACCESS</div><h2>手机扫码访问</h2></div><span class="muted">无需复制地址</span></div>
          <div class="access-content">
            {#if qrData}<img class="qr" src={qrData} alt="手机访问二维码" />{:else}<div class="qr-placeholder">二维码生成中</div>{/if}
            <div class="access-copy"><label for="address-select">选择访问地址</label><select id="address-select" bind:value={selectedAddress} on:change={refreshQr}>{#each node?.addresses ?? [] as address}<option value={address.url}>{address.label}</option>{/each}</select><code>{mobileUrl || '等待服务启动…'}</code><p>让手机连接同一网络后扫描二维码。</p></div>
          </div>
        </div>
      </section>

      <section class="panel clipboard-panel">
        <div class="panel-heading"><div><div class="section-kicker">LIVE CLIPBOARD</div><h2>共享剪贴板</h2></div><span class="muted">{clipboard ? `${clipboard.source_name} · ${formatTime(clipboard.updated_at_ms)}` : '复制后自动同步'}</span></div>
        <div class="clipboard-preview">
          <textarea id="clipboard-editor" bind:value={clipboardDraft} placeholder="在任意设备复制文字或图片，这里会显示最近内容"></textarea>
          {#if clipboard?.image_png}<img class="clipboard-image" src={`data:image/png;base64,${clipboard.image_png}`} alt="共享图片" />{/if}
        </div>
        <div class="button-row"><button class="primary" on:click={publishClipboard}>立即同步当前文字</button><span class="hint">桌面端复制文字或图片后会自动推送到在线设备的系统剪贴板。</span></div>
      </section>

      <section class="panel metrics-panel">
        <div class="panel-heading"><div><div class="section-kicker">HARDWARE MONITOR</div><h2>{selectedMetricsPeer ? selectedMetricsPeer.node_name + ' 的硬件状态' : '本机硬件状态'}</h2></div><span class="muted">{metricStatus(selectedMetricsPeer, currentMetrics)}</span></div>
        <div class="metrics-grid">
          <div class="metric-card"><span>CPU</span><strong>{formatPercent(currentMetrics?.cpu_usage_percent)}</strong><small>系统总使用率</small></div>
          <div class="metric-card"><span>内存</span><strong>{formatPercent(memoryPercent(currentMetrics))}</strong><small>{formatMemory(currentMetrics?.memory_used_bytes)} / {formatMemory(currentMetrics?.memory_total_bytes)}</small></div>
          <div class="metric-card"><span>GPU</span><strong>{gpuSummary(currentMetrics).split(' · ')[0]}</strong><small>{currentMetrics?.gpus.length ? currentMetrics.gpus.length + ' 个 GPU' : '未检测到可读指标'}</small></div>
          <div class="metric-card"><span>网络</span><strong>↓ {formatRate(currentMetrics?.network_down_bytes_per_sec)}</strong><small>↑ {formatRate(currentMetrics?.network_up_bytes_per_sec)}</small></div>
        </div>
        <div class="gpu-detail-list">
          {#each currentMetrics?.gpus ?? [] as gpu}
            <div class="gpu-detail-row">
              <div><strong>{gpu.name}</strong><span>{gpu.memory_kind === 'unified' ? '统一内存' : gpu.memory_kind === 'dedicated' ? '独立显存' : '显存类型未知'}</span></div>
              <div><b>{formatPercent(gpu.utilization_percent)}</b><small>{formatMemory(gpu.memory_used_bytes)} / {formatMemory(gpu.memory_total_bytes)}</small></div>
              {#if gpu.unavailable_reason}<em>{gpu.unavailable_reason}</em>{/if}
            </div>
          {:else}<div class="empty-state compact">GPU/显存指标暂不可用，CPU、内存和网络仍会继续采集。</div>{/each}
        </div>
        {#if selectedMetricsPeer}<button class="ghost small" on:click={() => (selectedMetricsPeer = null)}>查看本机</button>{/if}
      </section>

      <section class="panel devices-panel">
        <div class="panel-heading devices-heading"><div><div class="section-kicker">DEVICES</div><h2>在线设备</h2></div><div class="heading-actions"><span class="count-badge">{peers.length} 台</span><div class="manual-connect"><input bind:value={manualAddress} placeholder="手动输入节点地址" on:keydown={(event) => event.key === 'Enter' && connectManualPeer()} /><button class="ghost" on:click={connectManualPeer}>连接</button></div></div></div>
        <div class="peer-grid">
          {#each peers as peer}
            <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
            <article role="button" tabindex="0" class:dragging={dragPeer === peer.node_id} class="peer-card peer-target" on:click={() => startChoose(peer)} on:keydown={(event) => event.key === 'Enter' && startChoose(peer)} on:dragover|preventDefault={() => (dragPeer = peer.node_id)} on:dragleave={() => (dragPeer = '')} on:drop={(event) => onDrop(peer, event)}>
              <div class="peer-icon">{peer.node_name.slice(0, 1).toUpperCase()}</div>
              <div class="peer-copy"><strong>{peer.node_name}</strong><span>{peer.platform} · {peer.addresses[0]?.address ?? '在线'}</span><small class="peer-metrics-summary">{peerSummary(peer)}</small></div>
              <div class="online-mark"></div>
              <div class="peer-actions"><button on:click|stopPropagation={() => startChoose(peer)}>选择文件</button><button class="ghost" on:click|stopPropagation={() => startChoose(peer, true)}>选择文件夹</button><button class="ghost" on:click|stopPropagation={() => selectMetricsPeer(peer)}>查看监控</button></div>
              {#if dragPeer === peer.node_id}<div class="drop-overlay">松开即可发送</div>{/if}
            </article>
          {:else}<div class="empty-state">没有其他在线设备。启动另一台 MeshShare 后，它会自动出现在这里。</div>{/each}
        </div>
      </section>

      <section class="panel transfers-panel">
        <div class="panel-heading"><div><div class="section-kicker">TRANSFERS</div><h2>传输任务</h2></div><span class="muted">文件只在两台设备之间直传</span></div>
        <div class="transfer-list">
          {#each transfers.slice(0, 8) as transfer}
            <article class="transfer-row">
              <div class="transfer-icon">{transfer.direction === 'receive' ? '↓' : '↑'}</div>
              <div class="transfer-copy"><strong>{transfer.offer?.source_name ?? '文件传输'}</strong><span>{transfer.offer?.items.map((item) => item.name).join('、') ?? `${transfer.completed_items}/${transfer.total_items} 个项目`}</span></div>
              <div class="transfer-progress"><div class="progress-track"><div style={`width: ${transfer.total_bytes ? Math.min(100, transfer.completed_bytes / transfer.total_bytes * 100) : 0}%`}></div></div><span>{transfer.status === 'waiting' ? '等待确认' : transfer.status === 'completed' ? '已完成' : transfer.status === 'failed' ? transfer.error ?? '失败' : `${formatSize(transfer.completed_bytes)} / ${formatSize(transfer.total_bytes)}`}</span></div>
              {#if transfer.direction === 'receive' && transfer.status === 'waiting'}<div class="transfer-actions"><button class="primary small" on:click={() => acceptTransfer(transfer)}>接收</button><button class="ghost small" on:click={() => rejectTransfer(transfer)}>拒绝</button></div>{/if}
            </article>
          {:else}<div class="empty-state compact">还没有传输任务。</div>{/each}
        </div>
      </section>
    </main>
  {/if}

  <footer><span>MeshShare · 受信任局域网内的直接共享</span>{#if !isMobile}<span>关闭窗口后服务仍在系统托盘运行</span>{/if}</footer>
  {/if}
</div>

<input bind:this={fileInput} class="hidden-input" type="file" multiple on:change={onFileInput} />
<input bind:this={folderInput} class="hidden-input" type="file" multiple on:change={onFileInput} />

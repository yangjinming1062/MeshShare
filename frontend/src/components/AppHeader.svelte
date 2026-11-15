<script lang="ts">
  import Icon from './Icon.svelte';
  import TogglePill from './TogglePill.svelte';
  import { platformLabel } from '../format';
  import type { NodeInfo } from '../types';

  export let node: NodeInfo | null = null;
  export let status = '';
  export let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  export let syncEnabled = true;
  export let autostartEnabled = true;
  export let autostartBusy = false;
  export let remote = false;
  export let remotePath = '';
  export let onBack: () => void = () => {};
  export let onSync: () => void = () => {};
  export let onAutostart: () => void = () => {};
  export let onSettings: () => void = () => {};
</script>

<header class="app-header">
  {#if remote}
    <button type="button" class="btn-icon" aria-label="返回本机" title="返回本机" on:click={onBack}>
      <Icon name="arrow-left" size={17} />
    </button>
  {/if}
  <span class="node-status" data-kind={statusKind} aria-hidden="true">
    <span class="status-dot"></span>
  </span>
  <h1 title={node?.node_name}>{node?.node_name ?? 'MeshShare 节点'}</h1>
  {#if remote}<span class="chip remote-chip">远程管理</span>{/if}
  {#if remote && remotePath}<span class="remote-path" title={remotePath}>{remotePath}</span>{/if}
  <span class="status-text" class:sr-only={statusKind === 'ok'} role="status" aria-live="polite">{status}</span>
  {#if node}
    <div class="header-chips">
      <span class="chip">{platformLabel(node.platform)}</span>
      <span class="chip mono">:{node.port}</span>
      {#if node.addresses[0]}
        <span class="chip mono" title={node.addresses[0].address}>{node.addresses[0].address}</span>
      {/if}
    </div>
  {/if}
  <div class="header-actions">
    <TogglePill active={syncEnabled} label="自动同步" pressedLabel="同步已暂停" onToggle={onSync} />
    <TogglePill active={autostartEnabled} label="开机自启" pressedLabel="自启已关闭" disabled={autostartBusy} onToggle={onAutostart} />
    <button type="button" class="btn-icon" aria-label="设置" title="设置" on:click={onSettings}>
      <Icon name="settings" size={17} />
    </button>
  </div>
</header>

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
  export let settingsReady = false;
  export let settingsDisabled = false;
  export let remote = false;
  export let terminalCount = 0;
  export let terminalVisible = false;
  export let onTerminal: () => void = () => {};
  export let onBack: () => void = () => {};
  export let onSync: () => void = () => {};
  export let onAutostart: () => void = () => {};
  export let onSettings: () => void = () => {};
  export let onOpenProject: () => void = () => {};
  export let onCopyAddress: () => void = () => {};

  $: addressText = node?.addresses[0]
    ? (node.addresses[0].address.includes(':')
      ? '[' + node.addresses[0].address + ']:' + node.port
      : node.addresses[0].address + ':' + node.port)
    : '';
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
  <span class="status-text" class:sr-only={statusKind === 'ok'} role="status" aria-live="polite">{status}</span>
  {#if node}
    <div class="header-chips">
      <span class="chip">{platformLabel(node.platform)}</span>
      {#if node.addresses[0]}
        <button type="button" class="chip chip-button address-chip mono" title={'点击复制 ' + addressText} aria-label={'复制访问地址 ' + addressText} on:click={onCopyAddress}>
          <span class="address-text">{addressText}</span>
        </button>
      {:else}
        <span class="chip mono">:{node.port}</span>
      {/if}
    </div>
  {/if}
  <div class="header-actions">
    <button type="button" class="btn btn-sm" on:click={onTerminal} aria-pressed={terminalVisible}>
      <Icon name={terminalVisible ? 'monitor' : 'terminal'} size={16} />
      {terminalVisible ? '设备概览' : terminalCount ? '终端 · ' + terminalCount : '本机终端'}
    </button>
    <TogglePill active={syncEnabled} label="自动同步" pressedLabel="同步已暂停" disabled={settingsDisabled} onToggle={onSync} />
    <TogglePill active={autostartEnabled} label="开机自启" pressedLabel="自启已关闭" disabled={settingsDisabled} onToggle={onAutostart} />
    <button type="button" class="btn-icon" aria-label="设置" title="设置" disabled={!settingsReady} on:click={onSettings}>
      <Icon name="settings" size={17} />
    </button>
    <button type="button" class="btn-icon" aria-label="GitHub 项目主页" title="GitHub 项目主页" on:click={onOpenProject}>
      <Icon name="github" size={17} />
    </button>
  </div>
</header>

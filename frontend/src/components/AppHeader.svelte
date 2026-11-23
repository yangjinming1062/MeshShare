<script lang="ts">
  import Icon from './Icon.svelte';
  import TogglePill from './TogglePill.svelte';
  import { platformLabel } from '../format';
  import type { NodeInfo, PeerInfo } from '../types';

  export let node: NodeInfo | null = null;
  export let status = '';
  export let statusKind: 'ok' | 'warn' | 'error' = 'warn';
  export let syncEnabled = true;
  export let autostartEnabled = true;
  export let settingsReady = false;
  export let settingsDisabled = false;
  export let remoteTarget: PeerInfo | null = null;
  export let remoteAddress = '';
  export let remoteAddressLive = false;
  export let headless = false;
  export let serviceControlError = '';
  export let terminalCount = 0;
  export let terminalVisible = false;
  export let onTerminal: () => void = () => {};
  export let onBack: () => void = () => {};
  export let onSync: () => void = () => {};
  export let onAutostart: () => void = () => {};
  export let onSettings: (trigger: HTMLButtonElement) => void = () => {};
  export let onOpenProject: () => void = () => {};
  export let onCopyAddress: () => void = () => {};

  $: remoteName = node?.node_name ?? remoteTarget?.node_name ?? '';
  $: remotePlatform = headless ? 'Linux 无界面服务' : platformLabel(node?.platform ?? remoteTarget?.platform ?? '');
  $: localAddress = node?.addresses[0]
    ? (node.addresses[0].address.includes(':')
      ? '[' + node.addresses[0].address + ']:' + node.port
      : node.addresses[0].address + ':' + node.port)
    : '';
</script>

<header class="app-header" class:app-header--remote={!!remoteTarget && !terminalVisible}>
  {#if terminalVisible}
    <h1>终端工作区</h1>
    <div class="header-actions">
      {#if remoteTarget}
        <button type="button" class="btn btn-sm" on:click={onBack}>返回本机</button>
      {/if}
      <button type="button" class="btn btn-sm" title={remoteTarget ? '返回 ' + remoteName + ' 的远程管理' : undefined} on:click={onTerminal}>
        <Icon name="arrow-left" size={16} />
        {remoteTarget ? '返回远程管理' : '返回本机概览'}
      </button>
    </div>
  {:else}
    {#if remoteTarget}
      <button type="button" class="btn remote-back" on:click={onBack}>
        <Icon name="arrow-left" size={17} />
        返回本机
      </button>
      <div class="remote-identity">
        <h1 title={'远程管理 · ' + remoteName}>远程管理 · {remoteName}</h1>
        <div class="header-chips">
          <span class="chip">{remotePlatform}</span>
          {#if remoteAddress && remoteAddressLive}
            <button type="button" class="chip chip-button address-chip mono" title={'点击复制 ' + remoteAddress} aria-label={'复制访问地址 ' + remoteAddress} on:click={onCopyAddress}>
              <span class="address-text">{remoteAddress}</span>
            </button>
          {:else if remoteAddress}
            <span class="chip">上次连接 <span class="mono">{remoteAddress}</span></span>
          {/if}
          <span class="remote-status" role="status">
            <span class="node-status" data-kind={statusKind} aria-hidden="true"><span class="status-dot"></span></span>
            {status}
          </span>
        </div>
      </div>
    {:else}
      <span class="node-status" data-kind={statusKind} aria-hidden="true"><span class="status-dot"></span></span>
      <h1 title={node?.node_name}>{node?.node_name ?? 'MeshShare 节点'}</h1>
      <span class="chip">本机</span>
      <span class="status-text" class:sr-only={statusKind === 'ok'} role="status">{status}</span>
      {#if node}
        <div class="header-chips">
          <span class="chip">{platformLabel(node.platform)}</span>
          {#if localAddress}
            <button type="button" class="chip chip-button address-chip mono" title={'点击复制 ' + localAddress} aria-label={'复制访问地址 ' + localAddress} on:click={onCopyAddress}>
              <span class="address-text">{localAddress}</span>
            </button>
          {:else}
            <span class="chip mono">:{node.port}</span>
          {/if}
        </div>
      {/if}
    {/if}
    <div class="header-actions">
      <button type="button" class="btn btn-sm" disabled={!!remoteTarget && !node} on:click={onTerminal}>
        <Icon name="terminal" size={16} />
        {headless ? '目标终端' : remoteTarget ? '终端工作区' : terminalCount ? '终端 · ' + terminalCount : '本机终端'}
      </button>
      {#if settingsReady || !remoteTarget}
        <TogglePill active={syncEnabled} label={headless ? '允许发布' : '自动同步'} pressedLabel={headless ? '发布已暂停' : '同步已暂停'} disabled={settingsDisabled} onToggle={onSync} />
        {#if serviceControlError}
          <span class="chip" title={serviceControlError}>自启状态未知</span>
        {:else}
          <TogglePill active={autostartEnabled} label="开机自启" pressedLabel="自启已关闭" disabled={settingsDisabled} onToggle={onAutostart} />
        {/if}
      {/if}
      <button
        type="button" class="btn-icon"
        aria-label={remoteTarget ? '远程设置 · ' + remoteName : '本机设置'}
        title={remoteTarget ? '远程设置 · ' + remoteName : '本机设置'}
        disabled={!remoteTarget && !settingsReady}
        on:click={(event) => onSettings(event.currentTarget)}
      >
        <Icon name="settings" size={17} />
      </button>
      <button type="button" class="btn-icon" aria-label="GitHub 项目主页" title="GitHub 项目主页" on:click={onOpenProject}>
        <Icon name="github" size={17} />
      </button>
    </div>
  {/if}
</header>

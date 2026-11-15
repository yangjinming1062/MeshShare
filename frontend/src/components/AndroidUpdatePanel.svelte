<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import type { UpdateFrequency } from '../types';

  const labels = {
    unavailable: '当前构建无法更新',
    idle: '等待检查',
    disabled: '自动更新已关闭',
    checking: '正在检查',
    no_update: '暂无可用更新',
    waiting_for_transfers: '等待文件传输完成',
    waiting_for_network: '等待网络连接',
    metered: '等待非计费网络',
    downloading: '正在下载',
    permission: '需要安装权限',
    installing: '正在安装',
    paused: '更新已暂停',
    error: '更新失败',
  };

  type Status = {
    state: keyof typeof labels;
    frequency: UpdateFrequency;
    current_version: string;
    available_version: string | null;
    downloaded_bytes: number;
    content_length: number | null;
    error: string | null;
  };

  const updateActions: Partial<Record<Status['state'], string>> = {
    metered: '使用流量下载',
    permission: '允许安装',
    paused: '继续安装',
    error: '重试',
  };

  let status: Status | null = null;
  let commandError = '';
  let commandPending = false;
  $: busy = status?.state === 'checking' || status?.state === 'downloading' || status?.state === 'installing';
  $: actionLabel = status && updateActions[status.state];
  $: summary = status ? labels[status.state] : commandError ? '无法读取更新状态' : '正在载入';

  onMount(() => {
    let mounted = true;
    let receivedStatus = false;
    let unlisten: (() => void) | undefined;
    async function connect() {
      unlisten = await listen<Status>('android-update-status', ({ payload }) => {
        if (mounted) {
          receivedStatus = true;
          status = payload;
        }
      });
      if (!mounted) {
        unlisten();
        return;
      }
      const initial = await invoke<Status>('android_update_status');
      if (mounted && !receivedStatus) status = initial;
    }
    void connect().catch((error) => {
      if (mounted) commandError = String(error);
    });
    return () => {
      mounted = false;
      unlisten?.();
    };
  });

  async function runCommand(command: string, args?: { frequency: UpdateFrequency }) {
    commandPending = true;
    commandError = '';
    try {
      await invoke(command, args);
    } catch (error) {
      commandError = String(error);
    } finally {
      commandPending = false;
    }
  }

  async function changeFrequency(event: Event & { currentTarget: HTMLSelectElement }) {
    const select = event.currentTarget;
    await runCommand('android_set_update_frequency', { frequency: select.value as UpdateFrequency });
    if (commandError && status) select.value = status.frequency;
  }
</script>

<details class="phone-card update-card">
  <summary>
    <strong>应用更新</strong>
    <span class="muted" role="status">{summary}</span>
  </summary>
  <div class="update-body">
    {#if status}
      <p class="phone-copy">
        当前版本 v{status.current_version}{status.available_version ? ` · 新版本 v${status.available_version}` : ''}
      </p>
      {#if status.state === 'downloading' && status.content_length}
        <progress aria-label="更新包下载进度" max={status.content_length} value={status.downloaded_bytes}></progress>
      {:else if status.state === 'permission'}
        <p class="phone-copy">在系统设置中允许 MeshShare 安装应用，返回后会继续更新。</p>
      {/if}
      {#if status.state !== 'unavailable'}
        <div class="update-controls">
          <label>
            <span>检查频率</span>
            <select class="field" value={status.frequency} on:change={changeFrequency} disabled={commandPending || status.state === 'installing'}>
              <option value="weekly">每周</option>
              <option value="daily">每天</option>
              <option value="disabled">关闭</option>
            </select>
          </label>
          <button class="btn" type="button" disabled={busy || commandPending} on:click={() => runCommand('android_check_for_updates')}>立即检查</button>
        </div>
        {#if actionLabel}
          <button class="btn btn-primary" type="button" disabled={commandPending} on:click={() => runCommand('android_continue_update')}>{actionLabel}</button>
        {/if}
      {/if}
    {/if}
    {#if commandError || status?.error}
      <p class="update-error" role="alert">{commandError || status?.error}</p>
    {/if}
  </div>
</details>

<style>
  .update-card { display: block; padding-block: 0; }
  summary { min-height: 48px; padding-block: 12px; cursor: pointer; overflow-wrap: anywhere; }
  summary .muted { margin-inline-start: 8px; }
  .update-body { display: grid; gap: 12px; padding-block-end: 12px; }
  .update-controls { display: flex; flex-wrap: wrap; align-items: end; gap: 8px; }
  .update-controls label { flex: 1 1 112px; display: grid; gap: 4px; }
  .update-card button, .update-card select { min-height: 48px; font-size: inherit; }
  .update-body > button { justify-self: start; }
  progress { width: 100%; accent-color: var(--primary); }
  .update-error { color: var(--danger); margin: 0; overflow-wrap: anywhere; }
</style>

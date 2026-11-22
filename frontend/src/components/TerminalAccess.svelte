<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import TogglePill from './TogglePill.svelte';

  let enabled = false;
  let busy = true;
  let error = '';

  async function update(next?: boolean) {
    busy = true;
    error = '';
    try {
      enabled = next === undefined
        ? await invoke<boolean>('terminal_settings')
        : await invoke<boolean>('terminal_set_enabled', { enabled: next });
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  onMount(() => { void update(); });
</script>

<div class="setting-block">
  <div class="setting">
    <div class="setting-copy">
      <strong>允许其他设备操作本机终端</strong>
      <span>已连接的电脑和 Android 设备可以用你的系统账号运行命令。关闭会断开所有入站终端。</span>
    </div>
    <div class="setting-actions">
      <TogglePill active={enabled} ariaLabel="允许其他设备操作本机终端" label="已允许" pressedLabel="已关闭" disabled={busy} onToggle={() => update(!enabled)} />
    </div>
  </div>
  {#if error}<span class="hint" role="alert">{error}</span>{/if}
</div>

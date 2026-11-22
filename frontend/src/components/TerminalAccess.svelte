<script lang="ts">
  import { onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import Dialog from './Dialog.svelte';
  import TogglePill from './TogglePill.svelte';
  import type { PairRequest, TerminalIdentity, TerminalSettings, TrustDirection } from '../terminal';
  import type { PeerInfo } from '../types';

  export let open = false;
  export let connecting = false;
  export let peers: PeerInfo[] = [];
  let settings: TerminalSettings | null = null;
  let busy = false;
  let problem = '';
  let timer: number | undefined;
  let read: Promise<void> | null = null;
  let destroyed = false;
  let revoke: { direction: TrustDirection; identity: TerminalIdentity } | null = null;
  const sections: { direction: TrustDirection; title: string }[] = [
    { direction: 'incoming', title: '可操作本机的设备' },
    { direction: 'outgoing', title: '本机信任的目标' },
  ];
  let pending: PairRequest | undefined;
  let expired = false;

  // 核对过程中固定请求 ID。即使另一个请求先排到队首，也不替换正在核对的指纹。
  function syncPending(requests: PairRequest[]) {
    if (!pending) {
      pending = requests[0];
      expired = false;
    } else if (!requests.some((request) => request.id === pending?.id)) {
      if (requests.length) expired = true;
      else { pending = undefined; expired = false; }
    }
  }
  $: if (open || connecting) void refresh();
  function label(id: string) { return peers.find((p) => p.node_id === id)?.node_name ?? id; }

  async function readSettings() {
    try {
      const snapshot = await invoke<TerminalSettings>('terminal_settings');
      if (destroyed) return;
      settings = snapshot;
      syncPending(snapshot.pending);
    } catch (error) {
      problem = String(error);
    }
  }

  async function refresh(afterMutation = false): Promise<void> {
    if (destroyed) return;
    window.clearTimeout(timer);
    if (read) {
      await read;
      if (afterMutation) await refresh();
      return;
    }
    read = readSettings();
    await read;
    read = null;
    if (destroyed) return;
    const pairing = (settings?.pairing_until_ms ?? 0) > Date.now();
    if ((connecting && !settings?.error) || pairing || settings?.pending.length) {
      timer = window.setTimeout(() => void refresh(), 750);
    } else if (settings?.pairing_until_ms) {
      settings = { ...settings, pairing_until_ms: null };
    }
  }

  async function action(command: string, args?: Record<string, unknown>) {
    if (busy) return false;
    busy = true;
    problem = '';
    try {
      await invoke(command, args);
      return true;
    } catch (error) {
      problem = String(error);
      return false;
    } finally {
      await refresh(true);
      busy = false;
    }
  }

  async function decide(request: PairRequest | undefined, allow: boolean) {
    if (!request || busy) return;
    if (!expired && !await action('terminal_decide', { id: request.id, allow })) return;
    pending = undefined;
    if (settings) syncPending(settings.pending.filter((other) => other.id !== request.id));
  }

  async function forget() {
    if (revoke && await action('terminal_revoke', { direction: revoke.direction, nodeId: revoke.identity.node_id })) revoke = null;
  }

  onDestroy(() => {
    destroyed = true;
    window.clearTimeout(timer);
  });
</script>

{#if connecting && settings?.fingerprint && !open && !pending}
  <div class="fingerprint-bar"><span>首次配对时，请在目标机核对本机指纹</span><code>{settings.fingerprint}</code></div>
{/if}

<Dialog {open} title="本机终端授权" onClose={() => open = false}>
  <div class="setting-block">
    <div class="setting">
      <div class="setting-copy"><strong>允许远程终端</strong><span>已配对的设备可以使用当前系统账号在本机运行命令。</span></div>
      <TogglePill active={settings?.enabled ?? false} label="已开启" pressedLabel="已关闭" disabled={!settings || busy} onToggle={() => action('terminal_set_enabled', { enabled: !settings?.enabled })} />
    </div>
    {#if settings?.fingerprint}
      <span class="hint">本机公钥指纹</span><code class="code-block">{settings.fingerprint}</code>
    {/if}
    {#if settings?.enabled}
      <div class="setting-foot">
        <span class="hint">{settings.pairing_until_ms ? '等待新设备配对，截止 ' + new Date(settings.pairing_until_ms).toLocaleTimeString() : '新设备首次连接前，在这里开启配对。'}</span>
        <button class="btn btn-sm" type="button" disabled={busy || !!settings.pairing_until_ms} on:click={() => action('terminal_pair')}>配对新设备</button>
      </div>
    {/if}
  </div>
  {#if settings}
    {#each sections as section}
      {#if settings[section.direction].length}
        <div class="setting-block">
          <strong>{section.title}</strong>
          {#each settings[section.direction] as identity (identity.node_id)}
            <div class="setting-foot">
              <div class="setting-copy"><strong>{label(identity.node_id)}</strong><span>{identity.fingerprint}</span></div>
              <button class="btn btn-sm" type="button" disabled={busy} on:click={() => revoke = { direction: section.direction, identity }}>撤销</button>
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  {/if}
  {#if settings?.error || problem}
    <p class="hint" role="alert">{problem || settings?.error}</p>
  {/if}
  <svelte:fragment slot="footer"><button class="btn" type="button" disabled={busy} on:click={() => { problem = ''; void refresh(); }}>刷新</button><button class="btn btn-primary" type="button" on:click={() => open = false}>完成</button></svelte:fragment>
</Dialog>

<Dialog open={!!pending} title={pending?.direction === 'incoming' ? '授权设备操作本机' : '验证远程终端身份'} onClose={() => decide(pending, false)}>
  {#if pending}
    <p class="confirm-text">{pending.direction === 'incoming' ? '请核对发起设备显示的本机指纹。允许后，这台设备可以使用你的系统账号运行命令。' : '请在目标机的「本机终端授权」中核对下方指纹。'}</p>
    <strong>{label(pending.node_id)}</strong><code class="code-block">{pending.fingerprint}</code>
    {#if settings?.fingerprint}<span class="hint">本机公钥指纹</span><code class="code-block">{settings.fingerprint}</code>{/if}
    {#if expired}<span role="status">该请求已结束。关闭后可核对下一台设备。</span>{/if}
    {#if problem}<span role="alert">{problem}</span>{/if}
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" disabled={busy} on:click={() => decide(pending, false)}>{expired ? '关闭' : '拒绝'}</button>
    <button type="button" class="btn btn-primary" disabled={busy || expired} on:click={() => decide(pending, true)}>指纹一致，允许配对</button>
  </svelte:fragment>
</Dialog>

<Dialog open={!!revoke} title="撤销终端授权" onClose={() => revoke = null}>
  {#if revoke}<p class="confirm-text">确定撤销「{label(revoke.identity.node_id)}」的授权？该方向的活动会话将断开，下次连接需要重新配对。</p>{/if}
  {#if problem}<span role="alert">{problem}</span>{/if}
  <svelte:fragment slot="footer"><button class="btn" type="button" disabled={busy} on:click={() => revoke = null}>取消</button><button class="btn btn-danger" type="button" disabled={busy} on:click={forget}>撤销</button></svelte:fragment>
</Dialog>

<style>
  .fingerprint-bar { display: flex; gap: 10px; flex-wrap: wrap; padding: 6px var(--pad); font-size: 12px; color: var(--ink-2); }
  .fingerprint-bar code { overflow-wrap: anywhere; user-select: text; }
</style>

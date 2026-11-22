<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { Terminal } from '@xterm/xterm';
  import { TerminalSession, type TerminalState, type TerminalTarget } from '../terminal';
  import { terminalTransport } from '../terminalNative';

  export let id: string;
  export let target: TerminalTarget;
  export let visible: boolean;
  export let mobile = false;
  export let canReconnect = true;
  export let onState: (state: TerminalState) => void;
  export let onReconnect: () => void;

  let container: HTMLDivElement;
  let state: TerminalState = 'connecting';
  let detail = '';
  let term: Terminal | undefined;
  const shortcuts = [{ label: 'Ctrl+C', data: '\x03' }, { label: 'Tab', data: '\t' }, { label: 'Esc', data: '\x1b' }];
  function sendShortcut(data: string) {
    if (state === 'connected') {
      term?.input(data, true);
      term?.focus();
    }
  }

  let updateSize: (() => void) | undefined;

  async function focusPane() {
    await tick();
    if (visible) {
      updateSize?.();
      term?.focus();
    }
  }
  $: if (visible && term) void focusPane();

  onMount(() => {
    let disposed = false;
    let observer: ResizeObserver | undefined;
    let session: TerminalSession | undefined;
    function changed(next: TerminalState, reason: string) {
      if (disposed) return;
      state = next;
      detail = reason;
      if (term) term.options.disableStdin = next !== 'connected';
      onState(next);
    }
    async function start() {
      const { createRenderer } = await import('../terminalRenderer');
      if (disposed) return;
      const renderer = createRenderer(container);
      term = renderer.term;
      session = new TerminalSession(terminalTransport(id, target.peerId), (data, ack) => renderer.term.write(data, ack), changed);
      updateSize = () => {
        if (!visible || !container.clientHeight || !container.clientWidth) return;
        const size = renderer.fit.proposeDimensions();
        if (size) renderer.term.resize(Math.min(1000, size.cols), Math.min(1000, size.rows));
      };
      updateSize();
      renderer.term.onResize(({ cols, rows }) => session?.resize(cols, rows));
      const encoder = new TextEncoder();
      renderer.term.onData((data) => session?.write(encoder.encode(data)));
      renderer.term.onBinary((data) => session?.write(Uint8Array.from(data, (char) => char.charCodeAt(0))));
      observer = new ResizeObserver(updateSize);
      observer.observe(container);
      await session.start(renderer.term.cols, renderer.term.rows);
    }
    void start().catch((error) => changed('ended', String(error)));
    return () => {
      disposed = true;
      observer?.disconnect();
      session?.dispose();
      term?.dispose();
    };
  });
</script>

<div class="terminal-pane" hidden={!visible}>
  <div class="terminal-screen" bind:this={container}></div>
  {#if mobile && state === 'connected'}
    <div class="shortcut-bar" aria-label="终端快捷键">
      {#each shortcuts as key}
        <button type="button" class="btn btn-sm" on:mousedown|preventDefault on:click={() => sendShortcut(key.data)}>{key.label}</button>
      {/each}
    </div>
  {/if}
  {#if state !== 'connected' || detail}
    <div class="terminal-status" role="status">
      <span>{state === 'connecting' ? (target.peerId ? '正在连接远程终端…' : '正在打开本机终端…') : state === 'ended' ? (target.peerId ? '已断开' : '会话已结束') : ''}{detail ? ' ' + detail : ''}</span>
      {#if state === 'ended' && canReconnect}<button type="button" class="btn btn-sm" on:click={onReconnect}>{target.peerId ? '重新连接' : '重新打开'}</button>{/if}
    </div>
  {/if}
</div>

<style>
  .terminal-pane { flex: 1; min-height: 0; min-width: 0; display: flex; flex-direction: column; background: #141923; }
  .terminal-pane[hidden] { display: none; }
  .terminal-screen { flex: 1; min-height: 0; overflow: hidden; margin: 12px 6px 10px 14px; }
  .terminal-screen :global(.xterm) { height: 100%; }
  .shortcut-bar { display: flex; gap: 6px; padding: 4px 8px; background: var(--surface-2); }
  .terminal-status { flex-shrink: 0; display: flex; gap: 12px; align-items: center; justify-content: space-between; padding: 8px 12px; color: var(--ink); background: var(--surface-2); font-size: 12px; }
  .terminal-status span { overflow-wrap: anywhere; }
  .terminal-status button { flex-shrink: 0; }
</style>

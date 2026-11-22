<script lang="ts">
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';
  import TerminalPane from './TerminalPane.svelte';
  import { terminalSessionId, type TerminalState, type TerminalTarget } from '../terminal';

  export let visible = false;
  export let count = 0;
  export let remoteConnecting = false;
  const local: TerminalTarget = { peerId: null, name: '本机' };
  type Tab = { id: string; target: TerminalTarget; label: string; state: TerminalState };
  let tabs: Tab[] = [];
  let active = '';
  let tabList: HTMLDivElement;

  $: count = tabs.length;
  $: remoteConnecting = tabs.some((tab) => tab.target.peerId && tab.state === 'connecting');

  export function openSession(target: TerminalTarget = local) {
    const base = target.peerId ? '远程 · ' + target.name : '本机';
    let label = base;
    let number = 1;
    while (tabs.some((tab) => tab.label === label)) label = base + ' · ' + ++number;
    const id = terminalSessionId();
    tabs = [...tabs, { id, target, label, state: 'connecting' }];
    selectTab(id);
  }

  export function showWorkspace() {
    if (!tabs.length) openSession();
    else selectTab(active);
  }

  function selectTab(id: string) {
    active = id;
    visible = true;
    void tick().then(() => tabList?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' }));
  }

  function updateState(id: string, state: TerminalState) {
    tabs = tabs.map((tab) => tab.id === id ? { ...tab, state } : tab);
  }

  function closeTab(id: string) {
    const index = tabs.findIndex((tab) => tab.id === id);
    tabs = tabs.filter((tab) => tab.id !== id);
    if (id === active) active = tabs[Math.min(index, tabs.length - 1)]?.id ?? '';
    if (!tabs.length) visible = false;
  }

  function tabKey(event: KeyboardEvent, index: number) {
    let next: number;
    switch (event.key) {
      case 'ArrowRight': next = (index + 1) % tabs.length; break;
      case 'ArrowLeft': next = (index + tabs.length - 1) % tabs.length; break;
      case 'Home': next = 0; break;
      case 'End': next = tabs.length - 1; break;
      default: return;
    }
    event.preventDefault();
    selectTab(tabs[next].id);
    void tick().then(() => tabList.querySelector<HTMLButtonElement>('[aria-selected="true"]')?.focus());
  }
</script>

<section class="terminal-workspace" aria-label="终端" hidden={!visible}>
  <div class="toolbar">
    <div class="tabs" role="tablist" aria-label="终端会话" bind:this={tabList}>
      {#each tabs as tab, index (tab.id)}
        <div class="session-tab" class:selected={tab.id === active} role="presentation">
          <button type="button" role="tab" id={'tab-' + tab.id} aria-selected={tab.id === active} aria-controls={'pane-' + tab.id} tabindex={tab.id === active ? 0 : -1} title={tab.label} on:click={() => selectTab(tab.id)} on:keydown={(event) => tabKey(event, index)}>
            <i class:remote={!!tab.target.peerId} class:ended={tab.state === 'ended'}></i>
            <span>{tab.label}</span>
          </button>
          <button type="button" class="btn-icon tab-close" aria-label={'关闭 ' + tab.label + ' 终端'} title="结束并关闭会话" on:click={() => closeTab(tab.id)}><Icon name="close" size={12} /></button>
        </div>
      {/each}
    </div>
    <button type="button" class="btn-icon" aria-label="新建本机终端" title="新建本机终端" on:click={() => openSession()}><Icon name="plus" size={16} /></button>
  </div>
  {#each tabs as tab (tab.id)}
    <div class="pane-wrapper" role="tabpanel" id={'pane-' + tab.id} aria-labelledby={'tab-' + tab.id} hidden={tab.id !== active}>
      <TerminalPane id={tab.id} target={tab.target} visible={visible && tab.id === active} onState={(state) => updateState(tab.id, state)} onReconnect={() => openSession(tab.target)} />
    </div>
  {/each}
</section>

<style>
  .terminal-workspace { flex: 1; display: flex; flex-direction: column; min-height: 0; margin: 0 var(--pad) var(--pad); overflow: hidden; border: 1px solid var(--line); border-radius: var(--r-lg); }
  .terminal-workspace[hidden], .pane-wrapper[hidden] { display: none; }
  .pane-wrapper { min-height: 0; display: flex; flex: 1; }
  .toolbar { min-height: 42px; display: flex; align-items: center; gap: 4px; padding: 4px 6px; border-bottom: 1px solid var(--line); background: var(--surface); }
  .tabs { display: flex; gap: 3px; max-width: calc(100% - 42px); overflow: auto; scrollbar-width: thin; }
  .session-tab { display: flex; align-items: center; padding: 0 4px; border-radius: var(--r-md); color: var(--ink-2); }
  .session-tab.selected { color: var(--primary); background: var(--primary-soft); }
  .session-tab button[role="tab"] { min-width: 0; display: flex; align-items: center; gap: 7px; padding: 7px 5px; font-size: 12px; white-space: nowrap; }
  .session-tab span { max-width: 165px; overflow: hidden; text-overflow: ellipsis; }
  .session-tab i { flex: 0 0 auto; width: 7px; height: 7px; border-radius: 50%; background: var(--primary); }
  .session-tab i.remote { background: var(--positive); }
  .session-tab i.ended { background: var(--muted); }
  .tab-close { height: 24px; width: 24px; }
</style>

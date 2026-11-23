<script lang="ts">
  import Icon from './Icon.svelte';
  import { canOpenTerminal } from '../terminal';
  import DeviceRow from './DeviceRow.svelte';
  import type { MobileSession, PeerInfo } from '../types';

  export let peers: PeerInfo[] = [];
  export let homeNodeId = '';
  export let mobileSessions: MobileSession[] = [];
  export let activePeerIds: Set<string> = new Set();
  export let savedAddresses: string[] = [];
  export let draggingKey = '';
  export let addOpen = false;
  export let manualAddress = '';
  export let onToggleAdd: () => void = () => {};
  export let onConnect: () => void = () => {};
  export let onConnectSaved: (address: string) => void = () => {};
  export let onMobileAccess: () => void = () => {};
  export let onSendPeer: (peer: PeerInfo) => void = () => {};
  export let onSendPeerFolder: (peer: PeerInfo) => void = () => {};
  export let onDetails: (peer: PeerInfo) => void = () => {};
  export let onTerminal: ((peer: PeerInfo) => void) | null = null;
  export let onOpenPeer: (peer: PeerInfo) => void = () => {};
  export let allowFolderSend = true;
  export let onSendMobile: (session: MobileSession) => void = () => {};
  export let onSendMobileFolder: (session: MobileSession) => void = () => {};
  export let onDragEnter: (key: string) => void = () => {};
  export let onDragLeave: (key: string) => void = () => {};
  export let onDropPeer: (peer: PeerInfo, event: DragEvent) => void = () => {};
  export let onDropMobile: (session: MobileSession, event: DragEvent) => void = () => {};

  $: total = peers.length + mobileSessions.length;
  $: hasTerminalActions = !!onTerminal && peers.some(canOpenTerminal);
</script>

<div class="devices-column">
  <section class="surface device-panel">
    <div class="surface-head">
      <div class="surface-title">
        <h2>在线设备</h2>
        <span class="chip">{total}</span>
      </div>
      <div class="surface-actions">
        <button type="button" class="btn-icon" aria-label="手动连接设备" title="手动连接设备" aria-expanded={addOpen} on:click={onToggleAdd}>
          <Icon name="plus" size={16} />
        </button>
        <button type="button" class="btn btn-sm" on:click={onMobileAccess}>
          <Icon name="phone" size={14} />
          浏览器访问
        </button>
      </div>
    </div>
    {#if addOpen}
      <div class="add-device">
        <input
          class="field"
          bind:value={manualAddress}
          placeholder="手动输入节点地址"
          aria-label="手动输入节点地址"
          on:keydown={(event) => event.key === 'Enter' && onConnect()}
        />
        <button type="button" class="btn btn-primary btn-sm" on:click={onConnect}>连接</button>
        <button type="button" class="btn btn-ghost btn-sm" on:click={onToggleAdd}>取消</button>
      </div>
      {#if savedAddresses.length}
        <div class="saved-addresses" aria-label="已保存的节点">
          <span class="muted">最近连接</span>
          {#each savedAddresses as address (address)}
            <button type="button" class="btn btn-ghost btn-sm mono" on:click={() => onConnectSaved(address)}>{address}</button>
          {/each}
        </div>
      {/if}
    {/if}
    <div class="surface-body">
      <ul class="device-list" class:device-list--with-terminal={hasTerminalActions} role="list">
        {#each peers as peer (peer.node_id)}
          <DeviceRow
            kind="peer"
            {peer}
            isHome={peer.node_id === homeNodeId}
            linkActive={activePeerIds.has(peer.node_id)}
            dragging={draggingKey === 'peer:' + peer.node_id}
            onSend={() => onSendPeer(peer)}
            onSendFolder={() => onSendPeerFolder(peer)}
            onDetails={() => onDetails(peer)}
            onOpen={() => onOpenPeer(peer)}
            onTerminal={onTerminal && canOpenTerminal(peer) ? () => onTerminal?.(peer) : null}
            {allowFolderSend}
            onDragOver={() => onDragEnter('peer:' + peer.node_id)}
            onDragLeave={() => onDragLeave('peer:' + peer.node_id)}
            onDropFile={(event) => onDropPeer(peer, event)}
          />
        {/each}
        {#each mobileSessions as session (session.session_id)}
          <DeviceRow
            kind="mobile"
            {session}
            dragging={draggingKey === 'mobile:' + session.session_id}
            onSend={() => onSendMobile(session)}
            onSendFolder={() => onSendMobileFolder(session)}
            onDragOver={() => onDragEnter('mobile:' + session.session_id)}
            onDragLeave={() => onDragLeave('mobile:' + session.session_id)}
            onDropFile={(event) => onDropMobile(session, event)}
          />
        {/each}
        {#if !total}
          <li class="empty">还没有其他设备在线。</li>
        {/if}
      </ul>
    </div>
  </section>
  <p class="panel-footnote">同一局域网中的 MeshShare 会自动出现；也可用 + 手动连接，或让手机用「浏览器访问」加入。</p>
</div>

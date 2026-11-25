<script lang="ts">
  import Icon from './Icon.svelte';
  import { canOpenRemoteSettings } from '../peers';
  import { canOpenTerminal } from '../terminal';
  import DeviceRow from './DeviceRow.svelte';
  import type { MobileSession, PeerInfo } from '../types';

  export let peers: PeerInfo[] = [];
  export let homeNodeId = '';
  export let mobileSessions: MobileSession[] = [];
  export let activePeerIds: Set<string> = new Set();
  export let draggingKey = '';
  export let onMobileAccess: () => void = () => {};
  export let onSendPeer: (peer: PeerInfo) => void = () => {};
  export let onSendPeerFolder: (peer: PeerInfo) => void = () => {};
  export let onDetails: (peer: PeerInfo) => void = () => {};
  export let onTerminal: ((peer: PeerInfo) => void) | null = null;
  export let onSettings: ((peer: PeerInfo, trigger: HTMLButtonElement) => void) | null = null;
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
  $: hasSettingsActions = !!onSettings && peers.some((peer) => canOpenRemoteSettings(peer, homeNodeId));
</script>

<div class="devices-column">
  <section class="surface device-panel">
    <div class="surface-head">
      <div class="surface-title">
        <h2>在线设备</h2>
        <span class="chip">{total}</span>
      </div>
      <div class="surface-actions">
        <button type="button" class="btn btn-sm" on:click={onMobileAccess}>
          <Icon name="phone" size={14} />
          浏览器访问
        </button>
      </div>
    </div>
    <div class="surface-body">
      <ul class="device-list" class:device-list--with-settings={hasSettingsActions} role="list">
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
            onSettings={onSettings && canOpenRemoteSettings(peer, homeNodeId) ? (trigger) => onSettings?.(peer, trigger) : null}
            onTerminal={onTerminal && canOpenTerminal(peer) ? () => onTerminal?.(peer) : null}
            settingsSlot={hasSettingsActions}
            terminalSlot={hasTerminalActions}
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
            settingsSlot={hasSettingsActions}
            terminalSlot={hasTerminalActions}
            onDragOver={() => onDragEnter('mobile:' + session.session_id)}
            onDragLeave={() => onDragLeave('mobile:' + session.session_id)}
            onDropFile={(event) => onDropMobile(session, event)}
          />
        {/each}
        {#if !total}
          <li class="empty">同一局域网内运行 MeshShare 的设备会自动出现在这里。</li>
        {/if}
      </ul>
    </div>
  </section>
</div>

<script lang="ts">
  import Icon from './Icon.svelte';
  import DeviceRow from './DeviceRow.svelte';
  import type { MobileSession, PeerInfo } from '../types';

  export let peers: PeerInfo[] = [];
  export let homeNodeId = '';
  export let mobileSessions: MobileSession[] = [];
  export let savedAddresses: string[] = [];
  export let draggingKey = '';
  export let addOpen = false;
  export let manualAddress = '';
  export let onToggleAdd: () => void = () => {};
  export let onConnect: () => void = () => {};
  export let onConnectSaved: (address: string) => void = () => {};
  export let onMobileAccess: () => void = () => {};
  export let showMobileAccess = true;
  export let showMobileSessions = true;
  export let onSendPeer: (peer: PeerInfo) => void = () => {};
  export let onSendPeerFolder: (peer: PeerInfo) => void = () => {};
  export let onDetails: (peer: PeerInfo) => void = () => {};
  export let onOpenPeer: (peer: PeerInfo) => void = () => {};
  export let allowOpen = true;
  export let onSendMobile: (session: MobileSession) => void = () => {};
  export let onSendMobileFolder: (session: MobileSession) => void = () => {};
  export let onDragEnter: (key: string) => void = () => {};
  export let onDragLeave: (key: string) => void = () => {};
  export let onDropPeer: (peer: PeerInfo, event: DragEvent) => void = () => {};
  export let onDropMobile: (session: MobileSession, event: DragEvent) => void = () => {};

  $: total = peers.length + (showMobileSessions ? mobileSessions.length : 0);
</script>

<section class="surface area-devices">
  <div class="surface-head">
    <div class="surface-title">
      <h2>在线设备</h2>
      <span class="chip">{total}</span>
    </div>
    <div class="surface-actions">
      <button type="button" class="btn-icon" aria-label="手动连接设备" title="手动连接设备" aria-expanded={addOpen} on:click={onToggleAdd}>
        <Icon name="plus" size={16} />
      </button>
      {#if showMobileAccess}
        <button type="button" class="btn btn-sm" on:click={onMobileAccess}>
          <Icon name="phone" size={14} />
          浏览器访问
        </button>
      {/if}
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
    <ul class="device-list" role="list">
      {#each peers as peer (peer.node_id)}
        <DeviceRow
          kind="peer"
          {peer}
          isHome={peer.node_id === homeNodeId}
          dragging={draggingKey === 'peer:' + peer.node_id}
          onSend={() => onSendPeer(peer)}
          onSendFolder={() => onSendPeerFolder(peer)}
          onDetails={() => onDetails(peer)}
          onOpen={() => onOpenPeer(peer)}
          {allowOpen}
          onDragOver={() => onDragEnter('peer:' + peer.node_id)}
          onDragLeave={() => onDragLeave('peer:' + peer.node_id)}
          onDropFile={(event) => onDropPeer(peer, event)}
        />
      {/each}
      {#if showMobileSessions}
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
      {/if}
      {#if !total}
        <li class="empty">没有其他在线设备。启动另一台 MeshShare，或让其他设备通过「浏览器访问」临时加入。</li>
      {/if}
    </ul>
  </div>
</section>

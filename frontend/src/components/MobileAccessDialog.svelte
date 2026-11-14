<script lang="ts">
  import Dialog from './Dialog.svelte';
  import type { AddressInfo } from '../types';

  export let open = false;
  export let nodeReady = false;
  export let addresses: AddressInfo[] = [];
  export let selectedAddress = '';
  export let mobileUrl = '';
  export let qrImage = '';
  export let secureUrl = '';
  export let caUrl = '';
  export let onSelect: (url: string) => void = () => {};
  export let onClose: () => void = () => {};
</script>

<Dialog {open} title="手机访问" width={420} {onClose}>
  {#if addresses.length}
    <div class="qr-wrap">
      {#if qrImage}
        <img class="qr" src={qrImage} alt="手机访问二维码" />
      {:else}
        <div class="qr-placeholder">二维码生成中</div>
      {/if}
    </div>
    <label class="field-label" for="address-select">选择访问地址</label>
    <select id="address-select" class="field" value={selectedAddress} on:change={(event) => onSelect(event.currentTarget.value)}>
      {#each addresses as address (address.url)}
        <option value={address.url}>{address.address}</option>
      {/each}
    </select>
    <div class="code-block">{mobileUrl}</div>
    <p class="hint">让手机连接同一网络后扫描二维码。</p>
    {#if secureUrl}
      <p class="hint">手机接收文件前请先安装 <a class="link" href={caUrl} target="_blank" rel="noreferrer">本节点 CA 证书</a>，再打开 HTTPS 地址。</p>
      <div class="code-block">{secureUrl + '/mobile'}</div>
    {/if}
  {:else}
    <div class="empty" role="status">
      {nodeReady ? '暂无可用的网络地址。连接局域网或 Tailscale 后会自动更新。' : '正在读取网络地址…'}
    </div>
  {/if}
  <svelte:fragment slot="footer">
    <button type="button" class="btn" on:click={onClose}>关闭</button>
  </svelte:fragment>
</Dialog>

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

  let caDownloadState: 'idle' | 'downloading' | 'success' | 'error' = 'idle';
  let caDownloadMessage = '';

  async function downloadCaCertificate() {
    if (!caUrl || caDownloadState === 'downloading') return;
    caDownloadState = 'downloading';
    caDownloadMessage = '正在下载 CA 证书…';
    try {
      const response = await fetch(caUrl, { cache: 'no-store' });
      if (!response.ok) throw new Error(`下载失败 (${response.status})`);
      const blob = await response.blob();
      if (!blob.size) throw new Error('节点返回的证书为空');
      const objectUrl = URL.createObjectURL(blob);
      const anchor = document.createElement('a');
      anchor.href = objectUrl;
      anchor.download = 'meshshare-ca.crt';
      anchor.style.display = 'none';
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      window.setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
      caDownloadState = 'success';
      caDownloadMessage = '证书已下载，请在系统或浏览器中完成安装。';
    } catch (error) {
      caDownloadState = 'error';
      caDownloadMessage = error instanceof Error ? error.message : '无法下载 CA 证书';
    }
  }
</script>

<Dialog {open} title="浏览器访问" width={420} {onClose}>
  {#if addresses.length}
    <div class="qr-wrap">
      {#if qrImage}
        <img class="qr" src={qrImage} alt="浏览器访问二维码" />
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
    <p class="hint">HTTP 地址无需安装证书，打开后即可临时加入并下载文件；手机也可以扫描二维码。HTTP 不提供链路加密和完整性保护，仅适用于受信任网络。</p>
    {#if secureUrl}
      <p class="hint">
        如需选择本地目录并直接写入，请先
        <button type="button" class="link" on:click={downloadCaCertificate} disabled={!caUrl || caDownloadState === 'downloading'}>
          {caDownloadState === 'downloading' ? '下载本节点 CA 证书中…' : '下载本节点 CA 证书'}
        </button>
        并完成安装，再打开 HTTPS 地址。
      </p>
      {#if caDownloadMessage}
        <p class="hint" role="status" aria-live="polite">{caDownloadMessage}</p>
      {/if}
      <div class="code-block">{secureUrl}</div>
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

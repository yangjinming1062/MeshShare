<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import Dialog from './Dialog.svelte';
  import { clientPlatform, clientPlatformName } from '../format';
  import type { ClientPlatform } from '../format';
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

  let caDownloadState: 'idle' | 'saving' | 'error' = 'idle';
  let caDownloadMessage = '';
  let caSavedPath = '';
  let caResultUrl = '';
  let guideOpen = false;

  const platform = clientPlatform(navigator.userAgent);
  const installSteps: Record<ClientPlatform, string[]> = {
    windows: [
      '双击保存的证书文件，选择「安装证书」。',
      '存储位置选择「当前用户」。',
      '选择「将所有的证书都放入下列存储」，再浏览到「受信任的根证书颁发机构」。',
      '完成后重启浏览器，再打开 HTTPS 地址。'
    ],
    macos: [
      '双击保存的证书文件，在「钥匙串访问」的「系统」钥匙串中找到 MeshShare Local CA。',
      '双击该证书并展开「信任」，把「使用此证书时」改为「始终信任」。',
      '输入登录密码确认后，重启浏览器再打开 HTTPS 地址。'
    ],
    linux: [
      'Debian/Ubuntu：复制到 /usr/local/share/ca-certificates/，再执行 sudo update-ca-certificates。',
      'Fedora：复制到 /etc/pki/ca-trust/source/anchors/，再执行 sudo update-ca-trust。',
      'Firefox 使用自己的信任库，需要在浏览器设置里单独导入该证书。',
      '完成后重启浏览器，再打开 HTTPS 地址。'
    ],
    other: [
      '在系统或浏览器的证书设置中导入保存的证书，并信任为证书颁发机构。',
      '完成后重启浏览器，再打开 HTTPS 地址。'
    ]
  };
  const installHint = installSteps[platform];

  // 切换访问地址后，上一个地址的保存结果不再适用。
  $: if (caUrl !== caResultUrl) {
    caResultUrl = caUrl;
    caDownloadState = 'idle';
    caDownloadMessage = '';
    caSavedPath = '';
    guideOpen = false;
  }

  async function saveCaCertificate() {
    if (!caUrl || caDownloadState === 'saving') return;
    caDownloadState = 'saving';
    caDownloadMessage = '';
    caSavedPath = '';
    guideOpen = false;
    try {
      const saved = await invoke<string | null>('save_ca_certificate', { url: caUrl });
      if (saved === null) {
        caDownloadState = 'idle';
        return;
      }
      caDownloadState = 'idle';
      caSavedPath = saved;
      guideOpen = true;
    } catch (error) {
      caDownloadState = 'error';
      caDownloadMessage = error instanceof Error ? error.message : String(error);
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
    <p class="hint">HTTP 地址无需安装证书，打开后即可临时加入并下载文件；手机也可以扫描二维码。浏览器加入后本窗口自动关闭。HTTP 不提供链路加密和完整性保护，仅适用于受信任网络。</p>
    {#if secureUrl}
      <p class="hint">
        如需选择本地目录并直接写入，请先
        <button type="button" class="link" on:click={saveCaCertificate} disabled={!caUrl || caDownloadState === 'saving'}>
          {caDownloadState === 'saving' ? '正在保存 CA 证书…' : '下载本节点 CA 证书'}
        </button>
        并完成安装，再打开 HTTPS 地址。
      </p>
      {#if caDownloadMessage}
        <p class="hint hint-danger" role="status" aria-live="polite">{caDownloadMessage}</p>
      {/if}
      {#if caSavedPath}
        <p class="hint" role="status" aria-live="polite">证书已保存到：</p>
        <div class="code-block">{caSavedPath}</div>
      {/if}
      <details class="install-guide" bind:open={guideOpen}>
        <summary>安装步骤（{clientPlatformName(platform)}）</summary>
        <ol>
          {#each installHint as step}
            <li>{step}</li>
          {/each}
        </ol>
      </details>
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

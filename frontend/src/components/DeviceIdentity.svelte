<script module lang="ts">
  import type { IconName } from '../icons';

  const platforms = new Map<string, { icon: IconName; label: string }>([
    ['macos', { icon: 'apple-outline', label: 'macOS' }],
    ['linux', { icon: 'penguin', label: 'Linux' }],
    ['windows', { icon: 'window-grid', label: 'Windows' }],
    ['android', { icon: 'phone', label: 'Android' }],
    ['ios', { icon: 'phone', label: 'iOS' }],
  ]);
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { HEADLESS_CAPABILITY } from '../peers';
  import type { PeerInfo } from '../types';

  export let name: string;
  export let peer: PeerInfo | null = null;
  export let browserSource: string | undefined = undefined;
  export let isHome = false;
  export let descriptionId: string | undefined = undefined;

  $: browser = browserSource !== undefined;
  $: platform = platforms.get(peer?.platform ?? '');
  $: icon = browser ? 'globe' : (platform?.icon ?? 'monitor');
  $: headless = peer?.platform === 'linux' && peer.capabilities.includes(HEADLESS_CAPABILITY);
  $: system = browser ? '浏览器' : headless ? 'Linux 无界面服务' : (platform?.label ?? '未知系统');
  $: platformText = isHome ? '本机 · ' + system : system;
  $: address = browser ? browserSource : (peer?.addresses[0]?.address ?? '在线');
  $: meta = address ? platformText + ' · ' + address : platformText;
</script>

<span class="device-icon" aria-hidden="true"><Icon name={icon} size={18} /></span>
<span class="device-id">
  <strong title={name}>{name}</strong>
  <span id={descriptionId} class="device-meta" title={meta}>
    <span class="device-platform">{platformText}</span>
    {#if address}<span class="device-address">· {address}</span>{/if}
  </span>
</span>

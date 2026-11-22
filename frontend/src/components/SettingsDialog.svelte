<script lang="ts">
  import Dialog from './Dialog.svelte';
  import TerminalAccess from './TerminalAccess.svelte';
  import TogglePill from './TogglePill.svelte';
  import { clipboardHistoryRetentionLabel, updateStatusLabel } from '../format';
  import type {
    ClipboardHistoryMode,
    ClipboardHistorySettings,
    NodeInfo,
    ReceiveMode,
    ReceiveSettings,
    UpdateFrequency,
    UpdateStatus,
  } from '../types';

  export let open = false;
  export let node: NodeInfo | null = null;
  export let updateFrequency: UpdateFrequency = 'weekly';
  export let updateStatus: UpdateStatus | null = null;
  export let settingsDisabled = false;
  export let localMode = true;
  export let receiveSettings: ReceiveSettings = { mode: 'confirm' };
  export let historySettings: ClipboardHistorySettings = { mode: 'count', count: 50, days: 30 };
  export let remoteFileAccess = false;
  export let remoteFileAccessBusy = false;
  export let onFrequency: (value: UpdateFrequency) => void = () => {};
  export let onCheckUpdates: () => void = () => {};
  export let onReceiveMode: (value: ReceiveMode) => void = () => {};
  export let onChooseDirectory: () => void = () => {};
  export let onHistoryMode: (value: ClipboardHistoryMode) => void = () => {};
  export let onHistoryValue: (value: number) => void = () => {};
  export let onRemoteFileAccess: (value: boolean) => void = () => {};
  export let onClose: () => void = () => {};

  $: historyValue = historySettings.mode === 'count' ? historySettings.count : historySettings.days;
  $: historyMax = historySettings.mode === 'count' ? 500 : 365;
  $: updateLocked = settingsDisabled || ['downloading', 'installing'].includes(updateStatus?.state ?? '');
  $: checkDisabled = settingsDisabled || ['checking', 'downloading', 'installing'].includes(updateStatus?.state ?? '');
</script>

<Dialog {open} title="设置" width={520} {onClose}>
  <div class="setting-block">
    <div class="setting">
      <div class="setting-copy">
        <strong>自动更新</strong>
        <span>只获取 GitHub 稳定版 Release；安装会等待文件传输与终端会话结束，系统包需要本机授权。</span>
      </div>
      <div class="setting-actions">
        <div class="segmented" role="radiogroup" aria-label="自动更新检查频率">
          <label><input type="radio" name="update-frequency" value="daily" checked={updateFrequency === 'daily'} disabled={updateLocked} on:change={() => onFrequency('daily')} />每天</label>
          <label><input type="radio" name="update-frequency" value="weekly" checked={updateFrequency === 'weekly'} disabled={updateLocked} on:change={() => onFrequency('weekly')} />每周</label>
          <label><input type="radio" name="update-frequency" value="disabled" checked={updateFrequency === 'disabled'} disabled={updateLocked} on:change={() => onFrequency('disabled')} />关闭</label>
        </div>
      </div>
    </div>
    <div class="setting-foot">
      <span class="update-status" data-state={updateStatus?.state ?? 'idle'}>{updateStatusLabel(updateStatus)}</span>
      <button type="button" class="btn btn-sm" disabled={checkDisabled} on:click={onCheckUpdates}>立即检查</button>
    </div>
  </div>

  <div class="setting-block">
    <div class="setting">
      <div class="setting-copy">
        <strong>文件接收行为</strong>
        <span>{receiveSettings.mode === 'auto'
          ? (receiveSettings.default_path ? '默认保存到 ' + receiveSettings.default_path : '需要先选择默认接收目录')
          : '收到文件后由你选择是否接收和保存位置。'}</span>
      </div>
      <div class="setting-actions">
        <div class="segmented" role="radiogroup" aria-label="文件接收行为">
          <label><input type="radio" name="receive-mode" value="confirm" checked={receiveSettings.mode === 'confirm'} disabled={settingsDisabled} on:change={() => onReceiveMode('confirm')} />每次确认</label>
          <label><input type="radio" name="receive-mode" value="auto" checked={receiveSettings.mode === 'auto'} disabled={settingsDisabled} on:change={() => onReceiveMode('auto')} />自动接收</label>
        </div>
        <button type="button" class="btn btn-ghost btn-sm" disabled={settingsDisabled} on:click={onChooseDirectory}>选择默认目录</button>
      </div>
    </div>
  </div>

  {#if localMode}
    {#if open}<TerminalAccess />{/if}
    <div class="setting-block">
      <div class="setting">
        <div class="setting-copy">
          <strong>远程文件访问</strong>
          <span>开启后，同一局域网或 Tailscale 网络中的其他设备可以在远程管理里浏览本机目录，并选择本机文件发给它们的目标；默认关闭。</span>
        </div>
        <div class="setting-actions">
          <TogglePill
            active={remoteFileAccess}
            label="已允许"
            pressedLabel="已关闭"
            disabled={remoteFileAccessBusy}
            onToggle={() => onRemoteFileAccess(!remoteFileAccess)}
          />
        </div>
      </div>
    </div>
  {/if}

  <div class="setting-block">
    <div class="setting">
      <div class="setting-copy">
        <strong>剪贴板历史保留</strong>
        <span>每台机器独立保存文字和 PNG；网页端只能查看和恢复。</span>
      </div>
      <div class="setting-actions">
        <div class="segmented" role="radiogroup" aria-label="剪贴板历史保留模式">
          <label><input type="radio" name="history-mode" value="count" checked={historySettings.mode === 'count'} disabled={settingsDisabled} on:change={() => onHistoryMode('count')} />按条数</label>
          <label><input type="radio" name="history-mode" value="days" checked={historySettings.mode === 'days'} disabled={settingsDisabled} on:change={() => onHistoryMode('days')} />按天数</label>
        </div>
        <input
          class="field setting-number"
          type="number"
          min="1"
          max={historyMax}
          value={historyValue}
          aria-label="剪贴板历史保留数量"
          disabled={settingsDisabled}
          on:change={(event) => onHistoryValue(Number(event.currentTarget.value))}
        />
        <span class="muted">{historySettings.mode === 'count' ? '条' : '天'}</span>
      </div>
    </div>
    <span class="hint">当前设置：{clipboardHistoryRetentionLabel(historySettings)}</span>
  </div>

  <div class="setting-block">
    <div class="setting">
      <div class="setting-copy">
        <strong>关于</strong>
        <span>版本 v{node?.version ?? '—'} · 协议 v{node?.protocol_version ?? '—'} · 端口 {node?.port ?? '—'}</span>
      </div>
    </div>
    <span class="hint">关闭窗口后节点服务仍在系统托盘运行。</span>
  </div>

  <svelte:fragment slot="footer">
    <button type="button" class="btn btn-primary" on:click={onClose}>完成</button>
  </svelte:fragment>
</Dialog>

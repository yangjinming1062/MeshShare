<script lang="ts">
  import Dialog from './Dialog.svelte';
  import TogglePill from './TogglePill.svelte';
  import { clipboardHistoryRetentionLabel, platformLabel, updateStatusLabel } from '../format';
  import type { ClipboardHistoryMode, NodeInfo, NodeSettings, ReceiveMode, UpdateFrequency } from '../types';

  export let open = false;
  export let node: NodeInfo | null = null;
  export let snapshot: NodeSettings | null = null;
  export let settingsDisabled = false;
  export let remote: { name: string; address: string; platform: string; headless: boolean } | null = null;
  export let connectionError = '';
  export let feedback: { text: string; error: boolean } | null = null;
  export let restartBusy = false;
  export let remoteFileAccess = false;
  export let remoteFileAccessBusy = false;
  export let onReconnect: () => void = () => {};
  export let onSync: () => void = () => {};
  export let onAutostart: () => void = () => {};
  export let onToggleMonitor: () => void = () => {};
  export let onToggleMonitorLock: () => void = () => {};
  export let onRestart: () => void = () => {};
  export let onFrequency: (value: UpdateFrequency) => void = () => {};
  export let onCheckUpdates: () => void = () => {};
  export let onReceiveMode: (value: ReceiveMode) => void = () => {};
  export let onChooseDirectory: () => void = () => {};
  export let onHistoryMode: (value: ClipboardHistoryMode) => void = () => {};
  export let onHistoryValue: (value: number) => void = () => {};
  export let onRemoteFileAccess: (value: boolean) => void = () => {};
  export let onAllowRemoteTerminal: (() => Promise<void>) | null = null;
  export let onClose: () => void = () => {};

  let termPermissionBusy = false;
  let termPermissionError = '';
  async function allowRemoteTerminal() {
    if (!onAllowRemoteTerminal || termPermissionBusy) return;
    termPermissionBusy = true;
    termPermissionError = '';
    try {
      await onAllowRemoteTerminal();
    } catch (error) {
      termPermissionError = error instanceof Error ? error.message : String(error);
    } finally {
      termPermissionBusy = false;
    }
  }

  $: headless = !!remote?.headless;
  $: subtitle = remote ? [headless ? 'Linux 无界面服务' : platformLabel(remote.platform), remote.address].filter(Boolean).join(' · ') : '';
  $: serviceControlError = snapshot?.service_control_error ?? '';
  $: updateStatus = snapshot?.update_status;
  $: updateLocked = settingsDisabled || ['downloading', 'installing'].includes(updateStatus?.state ?? '');
  $: checkDisabled = updateLocked || updateStatus?.state === 'checking';
</script>

<Dialog {open} title={remote ? '远程设置 · ' + remote.name : '本机设置'} {subtitle} remote={!!remote} width={520} {onClose}>
  {#if connectionError}
    <div class="settings-connection">
      <p class="hint-danger" role="alert">{connectionError}</p>
      <button type="button" class="btn btn-sm" on:click={onReconnect}>重新连接</button>
    </div>
  {:else if !snapshot}
    <p class="muted" role="status">{remote ? '正在读取 ' + remote.name + ' 的设置…' : '正在读取本机设置…'}</p>
  {:else}
    {@const receiveSettings = snapshot.receive_settings}
    {@const historySettings = snapshot.clipboard_history_settings}
    {@const historyValue = historySettings.mode === 'count' ? historySettings.count : historySettings.days}
    {#if remote}
      <div class="setting-block">
        <div class="setting">
          <div class="setting-copy"><strong>{headless ? '剪贴板发布' : '剪贴板自动同步'}</strong></div>
          <TogglePill
            active={snapshot.clipboard_sync_enabled}
            ariaLabel={headless ? '允许剪贴板发布' : '剪贴板自动同步'}
            label="已开启"
            pressedLabel="已暂停"
            disabled={settingsDisabled}
            onToggle={onSync}
          />
        </div>
        <div class="setting">
          <div class="setting-copy">
            <strong>开机自启</strong>
            {#if serviceControlError}<span>{serviceControlError}</span>{/if}
          </div>
          <TogglePill
            active={snapshot.autostart_enabled}
            ariaLabel="开机自启"
            label="已开启"
            pressedLabel="已关闭"
            disabled={settingsDisabled || !!serviceControlError}
            onToggle={onAutostart}
          />
        </div>
        {#if !headless}
          <div class="setting">
            <div class="setting-copy">
              <strong>悬浮监控条</strong>
              {#if !snapshot.monitor.capabilities.can_click_through}<span>目标桌面环境暂不支持点击穿透。</span>{/if}
            </div>
            <div class="setting-actions">
              <TogglePill
                active={snapshot.monitor.visible}
                ariaLabel="显示悬浮监控条"
                label="已显示"
                pressedLabel="已隐藏"
                disabled={settingsDisabled}
                onToggle={onToggleMonitor}
              />
              <TogglePill
                active={snapshot.monitor.locked}
                ariaLabel="悬浮监控条点击穿透"
                label="已穿透"
                pressedLabel="点击穿透"
                disabled={settingsDisabled || !snapshot.monitor.capabilities.can_click_through}
                onToggle={onToggleMonitorLock}
              />
            </div>
          </div>
        {/if}
      </div>
    {/if}

    <div class="setting-block">
      <div class="setting">
        <div class="setting-copy">
          <strong>自动更新</strong>
          <span>{headless ? '服务会在传输和终端空闲后自动安装签名的稳定版。' : '只获取 GitHub 稳定版 Release，安装会等待文件传输与终端会话结束，系统包需要在该节点授权。'}</span>
        </div>
        <div class="setting-actions">
          <div class="segmented" role="radiogroup" aria-label="自动更新检查频率">
            <label><input type="radio" name="update-frequency" value="daily" checked={snapshot.update_frequency === 'daily'} disabled={updateLocked} on:click|preventDefault={() => onFrequency('daily')} />每天</label>
            <label><input type="radio" name="update-frequency" value="weekly" checked={snapshot.update_frequency === 'weekly'} disabled={updateLocked} on:click|preventDefault={() => onFrequency('weekly')} />每周</label>
            <label><input type="radio" name="update-frequency" value="disabled" checked={snapshot.update_frequency === 'disabled'} disabled={updateLocked} on:click|preventDefault={() => onFrequency('disabled')} />关闭</label>
          </div>
        </div>
      </div>
      <div class="setting-foot">
        <span class="update-status" data-state={updateStatus?.state ?? 'idle'}>{updateStatusLabel(updateStatus ?? null)}</span>
        <button type="button" class="btn btn-sm" disabled={checkDisabled} on:click={onCheckUpdates}>立即检查</button>
      </div>
    </div>

    <div class="setting-block">
      <div class="setting">
        <div class="setting-copy">
          <strong>文件接收行为</strong>
          <span>{receiveSettings.mode === 'auto'
            ? (receiveSettings.default_path ? '默认保存到 ' + receiveSettings.default_path : '需要先选择默认接收目录')
            : receiveSettings.default_path
              ? '逐次确认，默认目录为 ' + receiveSettings.default_path
              : '收到文件后由你选择是否接收和保存位置。'}</span>
        </div>
        <div class="setting-actions">
          <div class="segmented" role="radiogroup" aria-label="文件接收行为">
            <label><input type="radio" name="receive-mode" value="confirm" checked={receiveSettings.mode === 'confirm'} disabled={settingsDisabled} on:click|preventDefault={() => onReceiveMode('confirm')} />每次确认</label>
            <label><input type="radio" name="receive-mode" value="auto" checked={receiveSettings.mode === 'auto'} disabled={settingsDisabled} on:click|preventDefault={() => onReceiveMode('auto')} />自动接收</label>
          </div>
          <button type="button" class="btn btn-ghost btn-sm" disabled={settingsDisabled} on:click={onChooseDirectory}>选择默认目录</button>
        </div>
      </div>
    </div>

    {#if !remote}
      {#if onAllowRemoteTerminal && node?.terminal && !node.terminal.host_fingerprint}
        <div class="setting-block">
          <div class="setting">
            <div class="setting-copy">
              <strong>远程终端授权</strong>
              <span>允许后，局域网或 Tailscale 中已连接的原生设备可以用当前系统账号运行命令。</span>
            </div>
            <button type="button" class="btn btn-sm" disabled={termPermissionBusy} on:click={allowRemoteTerminal}>允许远程终端</button>
          </div>
          {#if termPermissionError}<span role="alert">{termPermissionError}</span>{/if}
        </div>
      {/if}
      <div class="setting-block">
        <div class="setting">
          <div class="setting-copy">
            <strong>远程文件访问</strong>
            <span>开启后，同一局域网或 Tailscale 网络中的其他设备可以在远程管理里浏览本机目录，并选择本机文件发给它们的目标。默认关闭。</span>
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
          <span>每台机器独立保存文字和 PNG。网页端可查看和恢复历史。</span>
        </div>
        <div class="setting-actions">
          <div class="segmented" role="radiogroup" aria-label="剪贴板历史保留模式">
            <label><input type="radio" name="history-mode" value="count" checked={historySettings.mode === 'count'} disabled={settingsDisabled} on:click|preventDefault={() => onHistoryMode('count')} />按条数</label>
            <label><input type="radio" name="history-mode" value="days" checked={historySettings.mode === 'days'} disabled={settingsDisabled} on:click|preventDefault={() => onHistoryMode('days')} />按天数</label>
          </div>
          <input
            class="field setting-number"
            type="number"
            min="1"
            max={historySettings.mode === 'count' ? 500 : 365}
            value={historyValue}
            aria-label="剪贴板历史保留数量"
            disabled={settingsDisabled}
            on:change={(event) => {
              const input = event.currentTarget;
              const next = input.valueAsNumber;
              input.value = String(historyValue);
              onHistoryValue(next);
            }}
          />
          <span class="muted">{historySettings.mode === 'count' ? '条' : '天'}</span>
        </div>
      </div>
      <span class="hint">当前设置 {clipboardHistoryRetentionLabel(historySettings)}</span>
    </div>

    <div class="setting-block">
      <div class="setting">
        <div class="setting-copy">
          <strong>关于</strong>
          <span>版本 v{node?.version ?? '—'} · 协议 v{node?.protocol_version ?? '—'} · 端口 {node?.port ?? '—'}</span>
        </div>
      </div>
      {#if headless}
        <div class="setting-foot">
          <span class="hint">{serviceControlError || '由目标机器的 systemd 管理。'}</span>
          <button type="button" class="btn btn-sm" disabled={settingsDisabled || restartBusy || !!serviceControlError} on:click={onRestart}>{restartBusy ? '正在请求…' : '重启节点服务'}</button>
        </div>
      {:else}
        <span class="hint">关闭窗口后节点服务仍在系统托盘运行。</span>
      {/if}
    </div>
  {/if}

  <svelte:fragment slot="footer">
    {#if feedback}<p class="settings-feedback" class:hint-danger={feedback.error} role={feedback.error ? 'alert' : 'status'}>{feedback.text}</p>{/if}
    <button type="button" class="btn btn-primary" on:click={onClose}>完成</button>
  </svelte:fragment>
</Dialog>

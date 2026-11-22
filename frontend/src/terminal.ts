export const TERMINAL_CAPABILITY = 'terminal.ssh.v2';

export function terminalSessionId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  // 部分 WebView 的应用协议不提供 secure-context randomUUID。
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-');
}

export type TerminalTarget = { peerId: string | null; name: string };
export type TerminalState = 'connecting' | 'connected' | 'ended';
export type TerminalEvent =
  | { type: 'data'; sequence: number; data: number[] }
  | { type: 'exit'; reason: string };

export type TrustDirection = 'incoming' | 'outgoing';
export type TerminalIdentity = { node_id: string; fingerprint: string };
export type PairRequest = TerminalIdentity & { id: string; direction: TrustDirection };
export type TerminalSettings = {
  enabled: boolean;
  fingerprint: string | null;
  pairing_until_ms: number | null;
  incoming: TerminalIdentity[];
  outgoing: TerminalIdentity[];
  pending: PairRequest[];
  error: string | null;
};

export interface TerminalTransport {
  open(cols: number, rows: number, receive: (event: TerminalEvent) => void): Promise<void>;
  write(data: Uint8Array): Promise<void>;
  resize(cols: number, rows: number): Promise<void>;
  ack(sequence: number): Promise<void>;
  close(): Promise<void>;
}

// 每个标签持有独立的输入队列；大段粘贴分块后仍与后续按键有序。
export class TerminalSession {
  private transport: TerminalTransport;
  private render: (data: Uint8Array, rendered: () => void) => void;
  private changed: (state: TerminalState, detail: string) => void;
  private state: TerminalState = 'connecting';
  private closed = false;
  private queued = 0;
  private writes: Promise<void> = Promise.resolve();
  private size: { cols: number; rows: number } | null = null;
  private resizing = false;

  constructor(
    transport: TerminalTransport,
    render: (data: Uint8Array, rendered: () => void) => void,
    changed: (state: TerminalState, detail: string) => void,
  ) {
    this.transport = transport;
    this.render = render;
    this.changed = changed;
  }

  async start(cols: number, rows: number) {
    try {
      await this.transport.open(cols, rows, (event) => {
        if (this.closed || this.state === 'ended') return;
        if (event.type === 'exit') {
          this.end(event.reason);
        } else {
          // 首帧可能先于 open 的 IPC 回复到达，TUI 此时已能发送终端查询。
          this.ready();
          this.render(Uint8Array.from(event.data), () => {
            if (!this.closed && this.state !== 'ended') {
              void this.transport.ack(event.sequence).catch((error) => this.fail(error));
            }
          });
        }
      });
      if (this.closed) {
        await this.transport.close();
      } else {
        this.ready();
      }
    } catch (error) {
      this.fail(error);
    }
  }

  private ready() {
    if (this.state !== 'connecting') return;
    this.state = 'connected';
    this.changed('connected', '');
    void this.applySize();
  }

  write(data: Uint8Array) {
    if (this.closed || this.state !== 'connected') return;
    if (this.queued + data.length > 1024 * 1024) {
      this.changed('connected', '这次输入过大，请分段粘贴。');
      return;
    }
    this.queued += data.length;
    this.writes = this.writes.then(async () => {
      for (let offset = 0; offset < data.length; offset += 8192) {
        if (this.closed || this.state !== 'connected') return;
        await this.transport.write(data.subarray(offset, offset + 8192));
      }
    }).catch((error) => this.fail(error)).finally(() => { this.queued -= data.length; });
  }

  resize(cols: number, rows: number) {
    this.size = { cols, rows };
    void this.applySize();
  }

  private async applySize() {
    if (this.resizing || this.closed || this.state !== 'connected') return;
    this.resizing = true;
    try {
      while (this.size && !this.closed && this.state === 'connected') {
        const size = this.size;
        this.size = null;
        await this.transport.resize(size.cols, size.rows);
      }
    } catch (error) {
      this.fail(error);
    } finally {
      this.resizing = false;
    }
  }

  private end(detail: string) {
    if (this.closed || this.state === 'ended') return;
    this.state = 'ended';
    this.changed('ended', detail);
  }

  private fail(error: unknown) {
    if (this.closed || this.state === 'ended') return;
    this.end(error instanceof Error ? error.message : String(error));
    void this.transport.close().catch(() => {});
  }

  dispose() {
    this.closed = true;
    void this.transport.close().catch(() => {});
  }
}

// 定时刷新等待当前这一次读。推送或写入共用下一次补读，后续事件不会延长先前的等待。
export function snapshotRead(read: () => Promise<void>) {
  let active: Promise<void> | undefined;
  let scheduled: Promise<void> | undefined;
  return {
    refresh(invalidate = false): Promise<void> {
      if (active && !invalidate) return active;
      if (scheduled) return scheduled;
      const run = async () => {
        if (scheduled !== next) return;
        scheduled = undefined;
        active = next;
        try {
          await read();
        } finally {
          if (active === next) active = undefined;
        }
      };
      const next: Promise<void> = (active ?? Promise.resolve()).then(run, run);
      scheduled = next;
      return next;
    },
    reset() {
      active = undefined;
      scheduled = undefined;
    },
  };
}

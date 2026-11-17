type DropEntry = FileSystemEntry & {
  isFile: boolean;
  isDirectory: boolean;
  file?: (callback: (file: File) => void) => void;
  createReader?: () => { readEntries: (callback: (entries: DropEntry[]) => void) => void };
};

export async function filesFromDataTransfer(data: DataTransfer): Promise<File[]> {
  const files: File[] = [];
  const items = Array.from(data.items);
  for (const item of items) {
    const entry = (item as DataTransferItem & { webkitGetAsEntry?: () => DropEntry | null }).webkitGetAsEntry?.();
    if (entry) await readDropEntry(entry, '', files);
  }
  return files.length ? files : Array.from(data.files);
}

async function readDropEntry(entry: DropEntry, prefix: string, files: File[]): Promise<void> {
  if (entry.isFile && entry.file) {
    const file = await new Promise<File>((resolve) => entry.file!(resolve));
    const relativePath = `${prefix}${file.name}`;
    Object.defineProperty(file, 'webkitRelativePath', { value: relativePath, configurable: true });
    files.push(file);
    return;
  }
  if (!entry.isDirectory || !entry.createReader) return;
  const reader = entry.createReader();
  const entries: DropEntry[] = [];
  while (true) {
    const batch = await new Promise<DropEntry[]>((resolve) => reader.readEntries(resolve));
    if (!batch.length) break;
    entries.push(...batch);
  }
  for (const child of entries) await readDropEntry(child, `${prefix}${entry.name}/`, files);
}

import type { ClipboardHistoryItem } from './types';

const SEPARATOR = /[\s·\-_/]/;

function normalize(value: string): string {
  return value.toLowerCase().replace(/\s+/g, ' ').trim();
}

/** 单词按字符子序列匹配：连续命中权重最高，字符串开头次之，分隔符之后再次之。 */
function tokenScore(token: string, text: string): number | null {
  let score = 0;
  let matched = 0;
  let last = -2;
  for (let index = 0; index < text.length && matched < token.length; index += 1) {
    if (text[index] !== token[matched]) continue;
    score += 1;
    if (index === last + 1) score += 3;
    else if (index === 0) score += 2;
    else if (SEPARATOR.test(text[index - 1])) score += 1;
    last = index;
    matched += 1;
  }
  return matched === token.length ? score : null;
}

export function fuzzyScore(query: string, text: string): number | null {
  const tokens = normalize(query).split(' ').filter(Boolean);
  if (!tokens.length) return 0;
  const haystack = normalize(text);
  let total = 0;
  for (const token of tokens) {
    const score = tokenScore(token, haystack);
    if (score === null) return null;
    total += score;
  }
  return total;
}

/** 搜索覆盖列表已有的文字预览、来源设备与内容类型，不读取完整正文。 */
function haystack(item: ClipboardHistoryItem): string {
  return [
    item.text_preview ?? '',
    item.source_name,
    item.has_image ? '图片' : '',
    item.has_text ? '文字' : '',
  ].join(' ');
}

export function filterClipboardHistory(items: ClipboardHistoryItem[], query: string): ClipboardHistoryItem[] {
  if (!normalize(query)) return items;
  const matches: Array<{ item: ClipboardHistoryItem; score: number }> = [];
  for (const item of items) {
    const score = fuzzyScore(query, haystack(item));
    if (score !== null) matches.push({ item, score });
  }
  matches.sort((left, right) => right.score - left.score || right.item.updated_at_ms - left.item.updated_at_ms);
  return matches.map((match) => match.item);
}

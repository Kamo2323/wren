// Pure text helpers shared by the pages and the mock. No imports, so scripts/unit-tests.mjs can load
// this file directly with Node's TypeScript type stripping.

const isWordChar = (c: string | undefined) => c !== undefined && /[\p{L}\p{N}'’]/u.test(c);

/**
 * Replace every whole-word occurrence of `from` with `to`, matching case-insensitively. Mirrors Rust
 * `dictionary::replace_whole_word` (what `history_fix_word` writes to the store): an occurrence typed
 * exactly like `from` gets `to` as written; one that differs in case and starts with a capital gets `to`
 * with a capital first letter.
 */
export function replaceWord(text: string, from: string, to: string): string {
  const needle = [...from];
  if (needle.length === 0) return text;
  const hay = [...text];
  const same = (a: string, b: string) => a === b || a.toLowerCase() === b.toLowerCase();
  let out = "";
  let i = 0;
  while (i < hay.length) {
    const end = i + needle.length;
    const fits =
      end <= hay.length &&
      (i === 0 || !isWordChar(hay[i - 1])) &&
      (end === hay.length || !isWordChar(hay[end])) &&
      needle.every((c, k) => same(hay[i + k], c));
    if (!fits) {
      out += hay[i];
      i += 1;
      continue;
    }
    const found = hay.slice(i, end).join("");
    const first = [...to][0];
    const capital = found !== from && /\p{Lu}/u.test(found[0] ?? "");
    out += capital && first !== undefined && /\p{Ll}/u.test(first) ? first.toUpperCase() + to.slice(first.length) : to;
    i = end;
  }
  return out;
}

/** Whitespace-separated words, as the store counts them. */
export function wordCount(text: string): number {
  const t = text.trim();
  return t ? t.split(/\s+/).length : 0;
}

/** Split `text` around case-insensitive matches of `query` so matches can be wrapped in <mark>. */
export function markMatches(text: string, query: string): Array<{ text: string; match: boolean }> {
  const q = query.trim();
  if (!q) return [{ text, match: false }];
  const escaped = q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return text
    .split(new RegExp(`(${escaped})`, "gi"))
    .filter(Boolean)
    .map((part) => ({ text: part, match: part.toLowerCase() === q.toLowerCase() }));
}

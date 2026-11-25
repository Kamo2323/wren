// Enhanced notes leaving Wren (spec docs/superpowers/specs/2026-11-25-notes-export-design.md): one walk over
// the Markdown tokens produces each output, so they all agree:
//  - emailHtml: an HTML fragment with inline styles (mail apps drop <style>; Outlook needs inline table borders)
//  - pdfDocument: a complete page for the PDF renderer
//  - plainText: clean text for plain-text fields (no **, #, |)
//  - buildDocx: a real Word file
// Raw HTML in the model's output is dropped, every text run is escaped, and only http(s)/mailto links survive.
// No DOM here, so these run in Node unit tests as well as in the app.
import { Lexer, type Token, type Tokens } from "marked";

export interface NoteMeta {
  /** Meeting title. */
  title: string;
  /** Meeting start, ISO UTC. */
  startedAtUtc: string;
  /** "Short" | "Medium" | "Long". */
  lengthLabel: string;
}

// ---------- shared ----------

function lex(md: string): Token[] {
  return new Lexer({ gfm: true, breaks: false }).lex(md);
}

const SAFE_HREF = /^(https?:\/\/|mailto:)/i;

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** marked's text tokens arrive HTML-escaped when `escaped`; plain outputs need the raw characters. */
function unescapeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#x27;/g, "'")
    .replace(/&amp;/g, "&");
}

function tokenText(t: { text?: string; escaped?: boolean }): string {
  const s = t.text ?? "";
  return t.escaped ? unescapeEntities(s) : s;
}

export function dateLine(meta: NoteMeta): string {
  const d = new Date(meta.startedAtUtc);
  const when = Number.isNaN(d.getTime())
    ? ""
    : `${d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short", year: "numeric" })}, ${d.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" })}`;
  return [when, `${meta.lengthLabel} notes`].filter(Boolean).join(" · ");
}

/** "Weekly sync – 25 Nov 2026 (Medium notes)". The save dialog strips characters files can't have. */
export function exportFileName(meta: NoteMeta): string {
  const d = new Date(meta.startedAtUtc);
  const day = Number.isNaN(d.getTime()) ? "" : ` – ${d.toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" })}`;
  const title = meta.title.trim() || "Meeting notes";
  return `${title}${day} (${meta.lengthLabel} notes)`;
}

// ---------- email / PDF HTML ----------

const FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif";
const S = {
  wrap: `font-family: ${FONT}; font-size: 14px; line-height: 1.5; color: #1f2328;`,
  title: "margin: 0 0 2px; font-size: 20px; line-height: 1.3; font-weight: 700; color: #1f2328;",
  meta: "margin: 0 0 16px; font-size: 12.5px; color: #656d76;",
  h1: "margin: 18px 0 6px; font-size: 18px; line-height: 1.3; font-weight: 700; color: #1f2328;",
  h2: "margin: 18px 0 6px; font-size: 16px; line-height: 1.3; font-weight: 700; color: #1f2328;",
  h3: "margin: 14px 0 4px; font-size: 14.5px; line-height: 1.3; font-weight: 700; color: #1f2328;",
  p: "margin: 0 0 10px;",
  list: "margin: 0 0 10px; padding-left: 22px;",
  li: "margin: 0 0 4px;",
  liTask: "margin: 0 0 4px; list-style-type: none;",
  quote: "margin: 0 0 14px; padding: 10px 14px; border-left: 3px solid #4f6bed; background: #f2f4fe; color: #1f2328;",
  table: "border-collapse: collapse; margin: 0 0 12px; font-size: 13.5px;",
  th: "border: 1px solid #c9d1d9; padding: 6px 10px; text-align: left; vertical-align: top; background: #f3f5f7; font-weight: 600;",
  td: "border: 1px solid #c9d1d9; padding: 6px 10px; text-align: left; vertical-align: top;",
  code: "font-family: Menlo, Consolas, monospace; font-size: 12.5px; background: #f3f5f7; padding: 1px 4px; border-radius: 3px;",
  pre: "font-family: Menlo, Consolas, monospace; font-size: 12.5px; background: #f3f5f7; padding: 10px 12px; border-radius: 6px; white-space: pre-wrap; margin: 0 0 10px;",
  a: "color: #0b62d6; text-decoration: underline;",
  hr: "border: none; border-top: 1px solid #d0d7de; margin: 14px 0;",
};

function inlineHtml(tokens: Token[] | undefined): string {
  let out = "";
  for (const t of tokens ?? []) {
    switch (t.type) {
      case "text":
        out += "tokens" in t && t.tokens?.length ? inlineHtml(t.tokens) : esc(tokenText(t as Tokens.Text));
        break;
      case "escape":
        out += esc(tokenText(t as Tokens.Escape));
        break;
      case "strong":
        out += `<strong>${inlineHtml((t as Tokens.Strong).tokens)}</strong>`;
        break;
      case "em":
        out += `<em>${inlineHtml((t as Tokens.Em).tokens)}</em>`;
        break;
      case "del":
        out += `<del>${inlineHtml((t as Tokens.Del).tokens)}</del>`;
        break;
      case "codespan":
        out += `<code style="${S.code}">${esc(unescapeEntities((t as Tokens.Codespan).text))}</code>`;
        break;
      case "br":
        out += "<br>";
        break;
      case "link": {
        const l = t as Tokens.Link;
        const label = inlineHtml(l.tokens);
        out += SAFE_HREF.test(l.href) ? `<a href="${esc(l.href)}" style="${S.a}">${label}</a>` : label;
        break;
      }
      case "image":
        out += esc((t as Tokens.Image).text ?? "");
        break;
      // "html" (raw HTML from the model) and anything unknown: dropped.
    }
  }
  return out;
}

function blocksHtml(tokens: Token[]): string {
  let out = "";
  for (const t of tokens) {
    switch (t.type) {
      case "heading": {
        const h = t as Tokens.Heading;
        const level = Math.min(Math.max(h.depth, 1), 3) as 1 | 2 | 3;
        const tag = `h${level + 1}`; // the note's own headings sit under the title
        out += `<${tag} style="${S[`h${level}`]}">${inlineHtml(h.tokens)}</${tag}>`;
        break;
      }
      case "paragraph": {
        const inner = inlineHtml((t as Tokens.Paragraph).tokens);
        if (inner.trim()) out += `<p style="${S.p}">${inner}</p>`; // e.g. only raw HTML, now dropped
        break;
      }
      case "text": {
        const tt = t as Tokens.Text;
        out += tt.tokens?.length ? inlineHtml(tt.tokens) : esc(tokenText(tt));
        break;
      }
      case "list": {
        const l = t as Tokens.List;
        const tag = l.ordered ? "ol" : "ul";
        const start = l.ordered && typeof l.start === "number" && l.start !== 1 ? ` start="${l.start}"` : "";
        out += `<${tag}${start} style="${S.list}">`;
        for (const item of l.items) {
          const box = item.task ? (item.checked ? "☑ " : "☐ ") : "";
          const inner = blocksHtml(item.tokens.filter((x) => x.type !== "checkbox"));
          out += `<li style="${item.task ? S.liTask : S.li}">${box}${inner.replace(/^<p style="[^"]*">([\s\S]*)<\/p>$/, "$1")}</li>`;
        }
        out += `</${tag}>`;
        break;
      }
      case "blockquote":
        out += `<blockquote style="${S.quote}">${blocksHtml((t as Tokens.Blockquote).tokens).replace(/(<p style=")margin: 0 0 10px;/g, "$1margin: 0 0 6px;")}</blockquote>`;
        break;
      case "table": {
        const tb = t as Tokens.Table;
        out += `<table cellpadding="0" cellspacing="0" style="${S.table}"><thead><tr>`;
        for (const c of tb.header) out += `<th style="${S.th}">${inlineHtml(c.tokens)}</th>`;
        out += "</tr></thead><tbody>";
        for (const row of tb.rows) {
          out += "<tr>";
          for (const c of row) out += `<td style="${S.td}">${inlineHtml(c.tokens)}</td>`;
          out += "</tr>";
        }
        out += "</tbody></table>";
        break;
      }
      case "code":
        out += `<pre style="${S.pre}">${esc((t as Tokens.Code).text)}</pre>`;
        break;
      case "hr":
        out += `<hr style="${S.hr}">`;
        break;
      // "space", "html", "def" and anything unknown: dropped.
    }
  }
  return out;
}

/** Self-contained HTML fragment with inline styles, headed by the meeting title and date. */
export function emailHtml(md: string, meta: NoteMeta): string {
  return (
    `<div style="${S.wrap}">` +
    `<h1 style="${S.title}">${esc(meta.title.trim() || "Meeting notes")}</h1>` +
    `<p style="${S.meta}">${esc(dateLine(meta))}</p>` +
    blocksHtml(lex(md)) +
    `</div>`
  );
}

/** A complete page for the PDF renderer: the same content, A4/Letter pages, nothing split mid-row. */
export function pdfDocument(md: string, meta: NoteMeta): string {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${esc(exportFileName(meta))}</title><style>
@page { margin: 18mm; }
html, body { margin: 0; padding: 0; background: #fff; }
tr, li, blockquote, pre { break-inside: avoid; page-break-inside: avoid; }
h1, h2, h3, h4 { break-after: avoid; page-break-after: avoid; }
table { width: 100%; }
</style></head><body>${emailHtml(md, meta)}</body></html>`;
}

// ---------- plain text ----------

function inlinePlain(tokens: Token[] | undefined): string {
  let out = "";
  for (const t of tokens ?? []) {
    switch (t.type) {
      case "text":
        out += "tokens" in t && t.tokens?.length ? inlinePlain(t.tokens) : tokenText(t as Tokens.Text);
        break;
      case "escape":
        out += tokenText(t as Tokens.Escape);
        break;
      case "strong":
      case "em":
      case "del":
        out += inlinePlain((t as Tokens.Strong).tokens);
        break;
      case "codespan":
        out += unescapeEntities((t as Tokens.Codespan).text);
        break;
      case "br":
        out += "\n";
        break;
      case "link": {
        const l = t as Tokens.Link;
        const label = inlinePlain(l.tokens);
        const href = l.href.replace(/^mailto:/i, "");
        out += SAFE_HREF.test(l.href) && label.trim() !== href ? `${label} (${href})` : label;
        break;
      }
      case "image":
        out += (t as Tokens.Image).text ?? "";
        break;
    }
  }
  return out;
}

function blocksPlain(tokens: Token[], indent = ""): string[] {
  const lines: string[] = [];
  const gap = () => {
    if (lines.length && lines[lines.length - 1] !== "") lines.push("");
  };
  for (const t of tokens) {
    switch (t.type) {
      case "heading":
        gap();
        lines.push(indent + inlinePlain((t as Tokens.Heading).tokens).trim());
        break;
      case "paragraph": {
        const text = inlinePlain((t as Tokens.Paragraph).tokens);
        if (!text.trim()) break;
        gap();
        for (const l of text.split("\n")) lines.push(indent + l);
        break;
      }
      case "text": {
        const tt = t as Tokens.Text;
        for (const l of (tt.tokens?.length ? inlinePlain(tt.tokens) : tokenText(tt)).split("\n")) lines.push(indent + l);
        break;
      }
      case "list": {
        gap();
        const l = t as Tokens.List;
        let n = typeof l.start === "number" ? l.start : 1;
        for (const item of l.items) {
          const mark = item.task ? (item.checked ? "☑ " : "☐ ") : l.ordered ? `${n++}. ` : "• ";
          const inner = blocksPlain(
            item.tokens.filter((x) => x.type !== "checkbox"),
            indent + " ".repeat(mark.length),
          ).filter((x) => x.trim() !== "");
          const [first = "", ...rest] = inner;
          lines.push(indent + mark + first.slice(indent.length + mark.length));
          lines.push(...rest);
        }
        break;
      }
      case "blockquote":
        gap();
        lines.push(...blocksPlain((t as Tokens.Blockquote).tokens, indent).filter((x, i, a) => !(x === "" && (i === 0 || i === a.length - 1))));
        break;
      case "table": {
        gap();
        const tb = t as Tokens.Table;
        const head = tb.header.map((c) => inlinePlain(c.tokens).trim());
        for (const row of tb.rows) {
          const cells = row.map((c) => inlinePlain(c.tokens).trim());
          const [first = "", ...rest] = cells;
          const extra = rest.map((v, i) => (v ? `${head[i + 1] || ""}: ${v}`.replace(/^: /, "") : "")).filter(Boolean);
          lines.push(`${indent}• ${first}${extra.length ? ` (${extra.join("; ")})` : ""}`);
        }
        break;
      }
      case "code":
        gap();
        for (const l of (t as Tokens.Code).text.split("\n")) lines.push(indent + "    " + l);
        break;
      case "hr":
        gap();
        break;
    }
  }
  return lines;
}

/** Clean text: the title, the date line, then the notes with bullets as "•", no Markdown symbols. */
export function plainText(md: string, meta: NoteMeta): string {
  const body = blocksPlain(lex(md));
  while (body.length && body[body.length - 1] === "") body.pop();
  while (body.length && body[0] === "") body.shift();
  return [meta.title.trim() || "Meeting notes", dateLine(meta), "", ...body].join("\n").replace(/\n{3,}/g, "\n\n") + "\n";
}

// ---------- Word ----------

/** A real .docx (zip) of the notes. `docx` is loaded on demand so it isn't in the app's start-up bundle. */
export async function buildDocx(md: string, meta: NoteMeta): Promise<Uint8Array> {
  const d = await import("docx");
  type Run = InstanceType<typeof d.TextRun> | InstanceType<typeof d.ExternalHyperlink>;
  type Style = { bold?: boolean; italics?: boolean; strike?: boolean; code?: boolean };
  const FONT_NAME = "Calibri";

  function runs(tokens: Token[] | undefined, st: Style = {}): Run[] {
    const out: Run[] = [];
    const text = (s: string, extra: Style = {}) => {
      const x = { ...st, ...extra };
      out.push(
        new d.TextRun({
          text: s,
          bold: x.bold,
          italics: x.italics,
          strike: x.strike,
          font: x.code ? "Consolas" : undefined,
        }),
      );
    };
    for (const t of tokens ?? []) {
      switch (t.type) {
        case "text":
          if ("tokens" in t && t.tokens?.length) out.push(...runs(t.tokens, st));
          else text(tokenText(t as Tokens.Text));
          break;
        case "escape":
          text(tokenText(t as Tokens.Escape));
          break;
        case "strong":
          out.push(...runs((t as Tokens.Strong).tokens, { ...st, bold: true }));
          break;
        case "em":
          out.push(...runs((t as Tokens.Em).tokens, { ...st, italics: true }));
          break;
        case "del":
          out.push(...runs((t as Tokens.Del).tokens, { ...st, strike: true }));
          break;
        case "codespan":
          text(unescapeEntities((t as Tokens.Codespan).text), { code: true });
          break;
        case "br":
          out.push(new d.TextRun({ text: "", break: 1 }));
          break;
        case "link": {
          const l = t as Tokens.Link;
          const label = inlinePlain(l.tokens);
          if (SAFE_HREF.test(l.href)) {
            out.push(
              new d.ExternalHyperlink({
                link: l.href,
                children: [new d.TextRun({ text: label, style: "Hyperlink", bold: st.bold, italics: st.italics })],
              }),
            );
          } else text(label);
          break;
        }
        case "image":
          text((t as Tokens.Image).text ?? "");
          break;
      }
    }
    return out;
  }

  type Block = InstanceType<typeof d.Paragraph> | InstanceType<typeof d.Table>;
  const cellBorder = { style: d.BorderStyle.SINGLE, size: 4, color: "C9D1D9" };

  function blocks(tokens: Token[], level = 0, inQuote = false): Block[] {
    const out: Block[] = [];
    const quote = inQuote
      ? {
          shading: { type: d.ShadingType.CLEAR, color: "auto", fill: "F2F4FE" },
          border: { left: { style: d.BorderStyle.SINGLE, size: 18, color: "4F6BED", space: 8 } },
          indent: { left: 200 },
        }
      : {};
    for (const t of tokens) {
      switch (t.type) {
        case "heading": {
          const h = t as Tokens.Heading;
          const lvl = [d.HeadingLevel.HEADING_1, d.HeadingLevel.HEADING_2, d.HeadingLevel.HEADING_3][Math.min(Math.max(h.depth, 1), 3) - 1];
          out.push(new d.Paragraph({ heading: lvl, children: runs(h.tokens), keepNext: true }));
          break;
        }
        case "paragraph":
          if (!inlinePlain((t as Tokens.Paragraph).tokens).trim()) break;
          out.push(new d.Paragraph({ children: runs((t as Tokens.Paragraph).tokens), spacing: { after: 120 }, ...quote }));
          break;
        case "text": {
          const tt = t as Tokens.Text;
          out.push(new d.Paragraph({ children: tt.tokens?.length ? runs(tt.tokens) : [new d.TextRun(tokenText(tt))], ...quote }));
          break;
        }
        case "list": {
          const l = t as Tokens.List;
          for (const item of l.items) {
            const parts = item.tokens.filter((x) => x.type !== "checkbox");
            const firstInline = parts.find((x) => x.type === "text" || x.type === "paragraph") as Tokens.Text | Tokens.Paragraph | undefined;
            const box = item.task ? [new d.TextRun(item.checked ? "☑ " : "☐ ")] : [];
            const children = [...box, ...(firstInline ? runs(firstInline.tokens) : [])];
            out.push(
              new d.Paragraph({
                children,
                spacing: { after: 60 },
                ...(item.task
                  ? { indent: { left: 360 * (level + 1) } }
                  : l.ordered
                    ? { numbering: { reference: "wren-numbers", level: Math.min(level, 2) } }
                    : { bullet: { level: Math.min(level, 2) } }),
              }),
            );
            const rest = parts.filter((x) => x !== firstInline);
            out.push(...blocks(rest, level + 1, inQuote));
          }
          break;
        }
        case "blockquote":
          out.push(...blocks((t as Tokens.Blockquote).tokens, level, true));
          break;
        case "table": {
          const tb = t as Tokens.Table;
          const cell = (tokens: Token[], header: boolean) =>
            new d.TableCell({
              children: [new d.Paragraph({ children: runs(tokens, header ? { bold: true } : {}) })],
              shading: header ? { type: d.ShadingType.CLEAR, color: "auto", fill: "F3F5F7" } : undefined,
              margins: { top: 60, bottom: 60, left: 100, right: 100 },
              borders: { top: cellBorder, bottom: cellBorder, left: cellBorder, right: cellBorder },
            });
          out.push(
            new d.Table({
              width: { size: 100, type: d.WidthType.PERCENTAGE },
              rows: [
                new d.TableRow({ tableHeader: true, children: tb.header.map((c) => cell(c.tokens, true)) }),
                ...tb.rows.map((r) => new d.TableRow({ cantSplit: true, children: r.map((c) => cell(c.tokens, false)) })),
              ],
            }),
          );
          out.push(new d.Paragraph({ children: [], spacing: { after: 120 } }));
          break;
        }
        case "code":
          for (const line of (t as Tokens.Code).text.split("\n"))
            out.push(new d.Paragraph({ children: [new d.TextRun({ text: line, font: "Consolas" })] }));
          break;
        case "hr":
          out.push(new d.Paragraph({ children: [], border: { bottom: { style: d.BorderStyle.SINGLE, size: 6, color: "D0D7DE", space: 4 } } }));
          break;
      }
    }
    return out;
  }

  const doc = new d.Document({
    creator: "Wren",
    title: exportFileName(meta),
    styles: { default: { document: { run: { font: FONT_NAME, size: 22 } } } },
    numbering: {
      config: [
        {
          reference: "wren-numbers",
          levels: [0, 1, 2].map((lvl) => ({
            level: lvl,
            format: d.LevelFormat.DECIMAL,
            text: `%${lvl + 1}.`,
            alignment: d.AlignmentType.START,
            style: { paragraph: { indent: { left: 360 * (lvl + 1), hanging: 260 } } },
          })),
        },
      ],
    },
    sections: [
      {
        children: [
          new d.Paragraph({ heading: d.HeadingLevel.TITLE, children: [new d.TextRun(meta.title.trim() || "Meeting notes")] }),
          new d.Paragraph({ children: [new d.TextRun({ text: dateLine(meta), color: "656D76" })], spacing: { after: 240 } }),
          ...blocks(lex(md)),
        ],
      },
    ],
  });
  return new Uint8Array(await d.Packer.toArrayBuffer(doc));
}

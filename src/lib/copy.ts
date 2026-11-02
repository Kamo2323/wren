// Copying dictated text (final review I-2). Inside Tauri the Rust side copies it, marked so Windows
// keeps it out of clipboard history (Win+V) and cloud clipboard — the WebView's clipboard API cannot
// do that, so it is used only in a plain browser (the mock UI for screenshots). No other file may
// touch navigator.clipboard or execCommand("copy") (scripts/unit-tests.mjs checks).

export interface CopyDeps {
  inTauri: boolean;
  /** The `copy_text` command: rejects when the text cannot be copied (e.g. empty). */
  rustCopy: (text: string) => Promise<unknown>;
  browserCopy: (text: string) => Promise<boolean>;
}

/** True when the text is on the clipboard. */
export async function copyTextWith(text: string, deps: CopyDeps): Promise<boolean> {
  if (deps.inTauri) {
    try {
      await deps.rustCopy(text);
      return true;
    } catch {
      return false;
    }
  }
  return deps.browserCopy(text);
}

/** Browser-only copy (the mock UI). Falls back to a hidden textarea if the async API is blocked. */
export async function browserCopy(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

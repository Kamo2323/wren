// Which popup states dismiss themselves, and after how long (pure, so scripts/unit-tests.mjs can
// check it). A "No sound" shown while still recording stays until the recording replaces it (M-1):
// in hands-free it would otherwise fade after 4 s while the recording runs on, × and all.
import type { PopupKind, PopupState } from "../lib/types";

export function dismissMsFor(state: PopupState, durations: Partial<Record<PopupKind, number>>): number | undefined {
  if (state.kind === "noSound" && state.recording) return undefined;
  return durations[state.kind];
}

// Shared constants from the Rust side (`get_constants`), fetched once per page. Starts with the known
// defaults so the first render never waits, then switches to the real values when they arrive.
import { useEffect, useState } from "react";
import type { UiConstants } from "./types";
import { DEFAULT_CONSTANTS, getConstants } from "./api";

let cached: UiConstants | null = null;
let pending: Promise<UiConstants> | null = null;

function load(): Promise<UiConstants> {
  if (!pending) {
    pending = getConstants()
      .then((c) => (cached = c))
      .catch(() => (cached = DEFAULT_CONSTANTS));
  }
  return pending;
}

export function useConstants(): UiConstants {
  const [c, setC] = useState<UiConstants>(cached ?? DEFAULT_CONSTANTS);
  useEffect(() => {
    let alive = true;
    void load().then((v) => alive && setC(v));
    return () => {
      alive = false;
    };
  }, []);
  return c;
}

// The recording popup. Lives in a 360 × 120 transparent, click-through, never-focused window; the
// pill sits bottom-centre and springs between sizes inside it. All states from spec §11.
//
// Timing contract with the session (Task 8): transient states dismiss themselves after the popup*Ms
// durations from `get_constants` (pill shrinks and fades over EXIT_MS). The session can hide the window
// at duration + EXIT_MS, or send { kind: "hidden" } at any time. Timestamps in PopupState are epoch ms.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent, type ReactNode } from "react";
import type { PopupKind, PopupState, UiConstants } from "../lib/types";
import { inTauri, onPopupLevel, onPopupState, popupCopyText, popupDiscard, popupOpenMain, popupOpenMicSettings, popupUndoLearned, subscribe } from "../lib/api";
import { useConstants } from "../lib/useConstants";
import { formatTimer } from "../lib/format";
import { POPUP_SPRING, springEasing, supportsLinearEasing } from "../lib/spring";
import { Check, CircleAlert, Copy, MicOff, Sparkles, Undo2, WrenMark, X } from "../components/Icon";
import { LevelBars, levelBus, type BarsMode } from "./LevelBars";
import { DEMO_SEQUENCE, samplePopupState, startMockLevels } from "./mockPopup";

/** How long each self-dismissing state stays, from the shared constants. */
export function dismissDurations(c: UiConstants): Partial<Record<PopupKind, number>> {
  return {
    pasted: c.popupPastedMs,
    nothing: c.popupNothingMs,
    noSound: c.popupNoSoundMs,
    notPasted: c.popupNotPastedMs,
    learned: c.popupLearnedMs,
    error: c.popupErrorMs,
  };
}
export const EXIT_MS = 200;
const HINT_MS = 3500;
const AFTER_ACTION_MS = 1100; // "Copied" / "Undone" stay this long

const PILL_H = 38;
const COLLAPSED_W = 40;
const COLLAPSED_H = 10;

type Local = null | "copied" | "undone";

const REC_KINDS: PopupKind[] = ["listening", "handsfree", "capWarning", "transcribing"];

/** Keep clicks from moving DOM focus: the popup window must never take focus. */
const noFocus = (e: MouseEvent) => e.preventDefault();

function useReducedMotion(): boolean {
  const q = "(prefers-reduced-motion: reduce)";
  const [reduced, setReduced] = useState(() => window.matchMedia(q).matches);
  useEffect(() => {
    const mq = window.matchMedia(q);
    const on = () => setReduced(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return reduced;
}

const spring = supportsLinearEasing() ? springEasing(POPUP_SPRING) : { easing: "cubic-bezier(0.34, 1.3, 0.64, 1)", ms: 460 };

export default function Popup() {
  const reduced = useReducedMotion();
  const params = new URLSearchParams(location.search);
  const mockKind = !inTauri ? (params.get("state") as PopupKind | "demo" | null) : null;
  const holdMock = params.get("hold") === "1";
  const constants = useConstants();
  const DISMISS_MS = useMemo(() => dismissDurations(constants), [constants]);

  const [state, setState] = useState<PopupState>({ kind: "hidden" });
  const [receivedAt, setReceivedAt] = useState(() => Date.now());
  const [shown, setShown] = useState(false);
  const [display, setDisplay] = useState<PopupState>({ kind: "hidden" });
  const [heard, setHeard] = useState(false);
  const [local, setLocal] = useState<Local>(null);
  const [now, setNow] = useState(() => Date.now());
  const [contentW, setContentW] = useState(90);
  const [hfSince, setHfSince] = useState(0); // when hands-free began (for the "Tap Ctrl + Win" hint)
  const contentRef = useRef<HTMLDivElement>(null);
  const timers = useRef<number[]>([]);
  const heardRef = useRef(false);

  const clearTimers = () => {
    timers.current.forEach((t) => window.clearTimeout(t));
    timers.current = [];
  };

  const stateRef = useRef<PopupState>({ kind: "hidden" });
  const apply = useCallback((next: PopupState) => {
    const prev = stateRef.current;
    stateRef.current = next;
    const wasRec = prev.kind === "listening" || prev.kind === "handsfree" || prev.kind === "capWarning";
    const isRec = next.kind === "listening" || next.kind === "handsfree";
    if (isRec && !wasRec) {
      // a new recording: wait for real audio before the bars go live
      levelBus.reset();
      heardRef.current = false;
      setHeard(false);
    }
    if (next.kind === "handsfree" && prev.kind !== "handsfree") setHfSince(Date.now());
    setState(next);
    setReceivedAt(Date.now());
    setLocal(null);
  }, []);

  // ---- wire up: real events, or the mock preview ----
  useEffect(() => {
    if (inTauri) {
      return subscribe([
        onPopupState(apply),
        onPopupLevel((v) => {
          levelBus.push(v);
          if (!heardRef.current && v > 0.02) {
            heardRef.current = true;
            setHeard(true);
          }
        }),
      ]);
    }
    // Browser preview only: lets scripts/screens.mjs drive state changes (e.g. the rAF check).
    (window as unknown as { __wrenPopup?: unknown }).__wrenPopup = { apply };
    if (!mockKind) return;
    const silent = params.get("silent") === "1";
    const stopLevels = silent ? () => {} : startMockLevels();
    if (!silent) {
      heardRef.current = true;
      setHeard(true);
    }
    const opts = { cpu: params.get("cpu") === "1", slow: params.get("slow") === "1", handsFree: params.get("hf") === "1" };
    if (mockKind !== "demo") {
      const id = window.setTimeout(() => {
        apply(samplePopupState(mockKind, opts));
        if (!silent) setHeard(true);
      }, 120);
      return () => {
        window.clearTimeout(id);
        stopLevels();
      };
    }
    let i = 0;
    let id = 0;
    const step = () => {
      const s = DEMO_SEQUENCE[i % DEMO_SEQUENCE.length];
      apply(samplePopupState(s.kind, opts));
      if (s.kind === "listening" || s.kind === "handsfree") {
        heardRef.current = true;
        setHeard(true);
      }
      i++;
      id = window.setTimeout(step, s.ms);
    };
    id = window.setTimeout(step, 300);
    return () => {
      window.clearTimeout(id);
      stopLevels();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [apply]);

  // ---- show / hide and self-dismiss ----
  useEffect(() => {
    clearTimers();
    if (state.kind === "hidden") {
      setShown(false);
      return;
    }
    setDisplay(state);
    setShown(true);
    const ms = DISMISS_MS[state.kind];
    if (ms != null && !(holdMock && !inTauri)) {
      timers.current.push(window.setTimeout(() => setShown(false), ms));
    }
    return clearTimers;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state, DISMISS_MS]);

  // ---- once the exit animation is over, drop the content so the bars' rAF loop and the CSS
  // animations (shimmer, breathing) stop while the popup is hidden ----
  useEffect(() => {
    if (shown) return;
    const id = window.setTimeout(() => setDisplay({ kind: "hidden" }), EXIT_MS + 40);
    return () => window.clearTimeout(id);
  }, [shown]);

  // ---- clock for timers / "Still working…" ----
  const ticking = display.kind === "handsfree" || display.kind === "capWarning" || display.kind === "transcribing";
  useEffect(() => {
    if (!ticking || !shown) return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 200);
    return () => window.clearInterval(id);
  }, [ticking, shown]);

  // ---- click actions (the session makes the window clickable for these states) ----
  const copyNotPasted = async () => {
    if (display.kind !== "notPasted" || local) return;
    try {
      await popupCopyText(display.text);
    } catch {
      apply({ kind: "error", message: "Couldn't copy it. Open Wren to copy." });
      return;
    }
    clearTimers();
    setLocal("copied");
    timers.current.push(window.setTimeout(() => setShown(false), AFTER_ACTION_MS));
  };
  const undoLearned = async () => {
    if (display.kind !== "learned" || local) return;
    try {
      await popupUndoLearned(display.entryId);
    } catch {
      apply({ kind: "error", message: "Couldn't undo. Open Wren to change it." });
      return;
    }
    clearTimers();
    setLocal("undone");
    timers.current.push(window.setTimeout(() => setShown(false), AFTER_ACTION_MS));
  };
  const discard = () => void popupDiscard().catch(() => {});
  const openMain = () => void popupOpenMain().catch(() => {});
  const openMicSettings = () => void popupOpenMicSettings().catch(() => {});

  // ---- content ----
  const k = display.kind;
  const isRec = REC_KINDS.includes(k);
  let content: ReactNode = null;
  let group = k as string;
  let tone = "default";
  let clickable: (() => void) | undefined;
  let countdownMs: number | undefined;

  if (isRec) {
    group = "rec";
    const cap = k === "capWarning";
    // a cap warning during hands-free keeps the hands-free look and its ×
    const handsfree = k === "handsfree" || (cap && display.mode === "handsFree");
    const transcribing = k === "transcribing";
    const slow = transcribing && now - display.sinceMs >= constants.popupStillWorkingAfterMs;
    const cpu = (k === "listening" || k === "handsfree") && display.cpu;
    const barsMode: BarsMode = transcribing ? "frozen" : heard ? "live" : "waiting";
    tone = handsfree ? "handsfree" : cap ? "warn" : "default";

    let lead: ReactNode = <WrenMark size={17} className="pill__mark" />;
    if (cap) {
      lead = (
        <span className="pill__time num" key="c">
          {formatTimer(display.remainingMs - (now - receivedAt))} left
        </span>
      );
    } else if (k === "handsfree") {
      lead = (
        <span className="pill__time num" key="t">
          {formatTimer(now - display.startedAtMs)}
        </span>
      );
    }

    content = (
      <>
        <span className="slot slot--lead">{lead}</span>
        {slow ? (
          <span className="shimmer-text" key="slow">
            Still working…
          </span>
        ) : (
          <LevelBars mode={barsMode} reducedMotion={reduced} />
        )}
        {cpu && <span className="cpu-badge">CPU</span>}
        {handsfree && (
          <button type="button" className="pill__close" aria-label="Discard this dictation" title="Discard" onClick={discard} onMouseDown={noFocus} tabIndex={-1}>
            <X size={14} strokeWidth={2.4} />
          </button>
        )}
      </>
    );
  } else if (k === "warming") {
    content = (
      <>
        <WrenMark size={17} className="pill__mark pill__mark--breathe" />
        <span className="pill__text">Warming up…</span>
      </>
    );
  } else if (k === "pasted") {
    tone = "success";
    content = (
      <svg className="tick" width="18" height="18" viewBox="0 0 18 18" aria-label="Pasted">
        <path d="M3.5 9.5 L7.4 13.2 L14.6 5" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    );
  } else if (k === "nothing") {
    tone = "muted";
    content = (
      <>
        <WrenMark size={17} className="pill__mark pill__mark--muted" />
        <span className="pill__text">Didn't catch that</span>
      </>
    );
  } else if (k === "noSound") {
    tone = "warn";
    clickable = openMicSettings; // spec §4: opens Windows' microphone privacy settings
    content = (
      <>
        <MicOff size={16} strokeWidth={2.2} className="pill__icon" />
        <span className="pill__text">No sound from your mic</span>
      </>
    );
  } else if (k === "notPasted") {
    tone = local ? "success" : "action";
    clickable = copyNotPasted;
    countdownMs = local ? undefined : DISMISS_MS.notPasted;
    content = local ? (
      <>
        <Check size={16} strokeWidth={2.6} className="pill__icon" />
        <span className="pill__text">Copied</span>
      </>
    ) : (
      <>
        <Copy size={15} strokeWidth={2.2} className="pill__icon" />
        <span className="pill__text">
          Not pasted <span className="pill__soft">· click to copy</span>
        </span>
      </>
    );
  } else if (k === "learned") {
    tone = local ? "muted" : "learned";
    countdownMs = local ? undefined : DISMISS_MS.learned;
    content = local ? (
      <>
        <Undo2 size={15} strokeWidth={2.2} className="pill__icon" />
        <span className="pill__text">Undone</span>
      </>
    ) : (
      <>
        <Sparkles size={15} strokeWidth={2.2} className="pill__icon" />
        <span className="pill__text">
          Learned <span className="pill__from">{display.from}</span>
          <span className="pill__arrow">→</span>
          <strong>{display.to}</strong>
        </span>
        <span className="pill__sep" />
        <button type="button" className="pill__action" onClick={undoLearned} onMouseDown={noFocus} tabIndex={-1}>
          Undo
        </button>
      </>
    );
  } else if (k === "error") {
    tone = "error";
    clickable = openMain;
    countdownMs = DISMISS_MS.error;
    content = (
      <>
        <CircleAlert size={16} strokeWidth={2.2} className="pill__icon" />
        <span className="pill__text pill__text--clip">{display.message}</span>
      </>
    );
  }

  const contentKey = `${group}:${local ?? ""}`;

  // ---- measure content so the pill can spring to the right width ----
  useLayoutEffect(() => {
    const el = contentRef.current;
    if (!el) return;
    const w = el.offsetWidth;
    if (w && Math.abs(w - contentW) > 0.5) setContentW(w);
  });
  useEffect(() => {
    const el = contentRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setContentW(el.offsetWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, [contentKey]);

  const showHint = shown && k === "handsfree" && hfSince > 0 && now - hfSince < HINT_MS;

  const padX = k === "pasted" ? 10 : isRec ? 13.5 : 15;
  const width = shown ? Math.max(PILL_H, Math.round(contentW + padX * 2)) : COLLAPSED_W;
  const height = shown ? PILL_H : COLLAPSED_H;
  const pillStyle: CSSProperties = {
    width,
    height,
    ["--spring" as string]: spring.easing,
    ["--spring-ms" as string]: `${reduced ? 1 : spring.ms}ms`,
  };

  return (
    <div className={`stage ${reduced ? "is-reduced" : ""}`}>
      <div className={`hint ${showHint ? "is-on" : ""}`} aria-hidden={!showHint}>
        Tap <kbd>Ctrl</kbd> + <kbd>Win</kbd> to finish
      </div>
      <div
        className={`pill pill--${tone} ${shown ? "is-shown" : "is-hidden"} ${clickable ? "is-clickable" : ""}`}
        style={pillStyle}
        data-kind={k}
        onClick={clickable}
        onMouseDown={noFocus}
        role={clickable ? "button" : undefined}
        aria-live="polite"
      >
        <div className="pill__content" ref={contentRef} key={contentKey}>
          {content}
        </div>
        {countdownMs != null && shown && (
          <span className="pill__countdown" key={`cd-${contentKey}-${receivedAt}`} style={{ animationDuration: `${countdownMs}ms` }} />
        )}
      </div>
    </div>
  );
}

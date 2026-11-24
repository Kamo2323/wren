// Home: calm greeting, waveform dictation activity hero (Time saved, Speaking speed, Current streak),
// and recent dictations in soft hairline cards with real app icons. Click a card to copy it.
import { useCallback, useEffect, useState } from "react";
import type { Stats, TranscriptionRow } from "../lib/types";
import type { ResolvedTheme } from "../lib/theme";
import { historyRecent, mainReady, onHistoryChanged, statsGet, subscribe } from "../lib/api";
import { retentionLabel } from "../lib/format";
import { useNewIds, useTranscriptActions } from "../lib/useTranscriptActions";
import { useConstants } from "../lib/useConstants";
import { WaveHero } from "../components/WaveHero";
import { ChordKeys, EmptyState } from "../components/Empty";
import { TranscriptCard } from "../components/TranscriptCard";
import { ArrowRight } from "../components/Icon";

const RECENT_HOME_LIMIT = 8;

let lastStats: Stats | null = null;
let lastRows: TranscriptionRow[] | null = null;

function EmptyRecent({ kept, isMac }: { kept: string; isMac: boolean }) {
  return (
    <EmptyState
      art={<ChordKeys />}
      title="Hold, speak, let go"
      note={`Your dictations show up here for ${kept}. Click one to copy it again.`}
    >
      {isMac
        ? "Hold the fn and Control keys while you talk. When you let go, Wren types it where your cursor is. Double-tap them for hands-free."
        : "Hold the left Ctrl and Windows keys while you talk. When you let go, Wren types it where your cursor is. Double-tap them for hands-free."}
    </EmptyState>
  );
}

export default function Home({
  typingWpm: typingWpmSetting,
  theme,
  onOpenHistory,
}: {
  typingWpm?: number;
  theme: ResolvedTheme;
  onOpenHistory: () => void;
}) {
  const constants = useConstants();
  const typingWpm = typingWpmSetting ?? constants.typingWpmDefault;
  const kept = retentionLabel(constants.retentionHours);
  const [stats, setStats] = useState<Stats | null>(lastStats);
  const [rows, setRows] = useState<TranscriptionRow[] | null>(lastRows);
  const [now, setNow] = useState(() => Date.now());
  const { onCopy, onDelete, onFix } = useTranscriptActions(setRows);

  const load = useCallback(async () => {
    const [s, r] = await Promise.all([
      statsGet().catch(() => null),
      historyRecent(RECENT_HOME_LIMIT).catch(() => []),
    ]);
    if (s) setStats(s);
    setRows(r);
    setNow(Date.now());
    requestAnimationFrame(() => requestAnimationFrame(mainReady));
  }, []);

  useEffect(() => {
    if (stats) lastStats = stats;
  }, [stats]);
  useEffect(() => {
    if (rows) lastRows = rows;
  }, [rows]);

  useEffect(() => {
    void load();
    const unlisten = subscribe(onHistoryChanged(() => void load()));
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => {
      unlisten();
      window.removeEventListener("focus", onFocus);
      window.clearInterval(tick);
    };
  }, [load]);

  const newIds = useNewIds(rows, "home");

  return (
    <div className="page page--home">
      <header className="home-heading">
        <h1 className="page__title">Hi, Maximilian.</h1>
      </header>

      <WaveHero stats={stats} typingWpm={typingWpm} theme={theme} />

      <section className="recent" aria-labelledby="recent-title">
        <div className="section-title">
          <h2 id="recent-title">Recent dictations</h2>
          <button type="button" className="bare" onClick={onOpenHistory}>
            View history <ArrowRight size={15} strokeWidth={2.1} aria-hidden="true" />
          </button>
        </div>

        {rows === null ? null : rows.length === 0 ? (
          <EmptyRecent kept={kept} isMac={constants.platform === "macos"} />
        ) : (
          <div className="day__cards">
            {rows.map((r) => (
              <TranscriptCard
                key={r.id}
                row={r}
                now={now}
                onCopy={onCopy}
                onDelete={onDelete}
                onFix={onFix}
                isNew={newIds.has(r.id)}
              />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

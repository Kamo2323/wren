// Shown when Quit is chosen while a meeting records (spec §5.5).
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { flushPendingNotes, meetingStopAndQuit } from "../../lib/meetings";

export function QuitConfirm({ onClose }: { onClose: () => void }) {
  const keepRef = useRef<HTMLButtonElement>(null);
  const retryRef = useRef<HTMLButtonElement>(null);
  const [stopping, setStopping] = useState(false);
  const [saveFailed, setSaveFailed] = useState(false);
  useEffect(() => {
    if (!stopping) (saveFailed ? retryRef : keepRef).current?.focus();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !stopping && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, stopping, saveFailed]);

  const quit = async (save: boolean) => {
    setStopping(true);
    if (save && !(await flushPendingNotes())) {
      setSaveFailed(true);
      setStopping(false);
      return;
    }
    try {
      await meetingStopAndQuit();
    } finally {
      onClose();
    }
  };
  return (
    <div className="modal-scrim" role="presentation">
      <div className="modal quit-confirm" role="alertdialog" aria-modal="true" aria-labelledby="quit-title" aria-describedby="quit-desc">
        <h2 className="modal__title" id="quit-title">
          Stop meeting notes and quit?
        </h2>
        <p className="modal__desc" id="quit-desc">
          {saveFailed
            ? "Your last edit to My notes couldn't be saved."
            : "A meeting is still being recorded. Wren will save what it has transcribed so far, then quit."}
        </p>
        <div className="modal__actions">
          <Button ref={keepRef} variant="ghost" onClick={onClose} disabled={stopping}>
            Keep recording
          </Button>
          {saveFailed && (
            <Button ref={retryRef} variant="secondary" disabled={stopping} onClick={() => void quit(true).catch(() => {})}>
              Try again
            </Button>
          )}
          <Button variant="danger" disabled={stopping} onClick={() => void quit(!saveFailed).catch(() => {})}>
            {stopping ? "Stopping…" : saveFailed ? "Quit anyway" : "Stop and quit"}
          </Button>
        </div>
      </div>
    </div>
  );
}

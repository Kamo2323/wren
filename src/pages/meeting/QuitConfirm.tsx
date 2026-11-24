// Shown when Quit is chosen while a meeting records (spec §5.5).
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/Button";
import { flushPendingNotes, meetingStopAndQuit } from "../../lib/meetings";

export function QuitConfirm({ onClose }: { onClose: () => void }) {
  const keepRef = useRef<HTMLButtonElement>(null);
  const [stopping, setStopping] = useState(false);
  useEffect(() => {
    keepRef.current?.focus();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !stopping && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, stopping]);
  return (
    <div className="modal-scrim" role="presentation">
      <div className="modal quit-confirm" role="alertdialog" aria-modal="true" aria-labelledby="quit-title" aria-describedby="quit-desc">
        <h2 className="modal__title" id="quit-title">
          Stop meeting notes and quit?
        </h2>
        <p className="modal__desc" id="quit-desc">
          A meeting is still being recorded. Wren will save what it has transcribed so far, then quit.
        </p>
        <div className="modal__actions">
          <Button ref={keepRef} variant="ghost" onClick={onClose} disabled={stopping}>
            Keep recording
          </Button>
          <Button
            variant="danger"
            disabled={stopping}
            onClick={() => {
              setStopping(true);
              // My notes' last edit is saved first; Wren quits once the meeting is saved, and the
              // dialog only closes if that fails.
              void flushPendingNotes()
                .then(() => meetingStopAndQuit())
                .catch(() => {
                  setStopping(false);
                  onClose();
                });
            }}
          >
            {stopping ? "Stopping…" : "Stop and quit"}
          </Button>
        </div>
      </div>
    </div>
  );
}

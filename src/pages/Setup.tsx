// First-run speech setup: what will be downloaded (and how big), progress while it installs, a clear
// error with a safe retry, and — only once the real engine has warmed up — a short "try it" check
// (microphone level, the keyboard shortcut, a box to dictate into).
import { useEffect, useState } from "react";
import type { EngineStatus, SetupPlan, SetupStatus } from "../lib/types";
import { accessibilityStatus, openAccessibilitySettings, popupOpenMicSettings } from "../lib/api";
import { useMicLevel } from "../lib/useMicLevel";
import { Button } from "../components/Button";
import { ChordKeys } from "../components/Empty";
import { Check, CircleAlert, Cpu, Download, Info, Keyboard, Mic, Pause, Play, RotateCw, ShieldCheck } from "../components/Icon";

const METER_SEGMENTS = 20;

/** 3_617_000_000 → "3.6 GB"; 486_000_000 → "486 MB" (decimal units, like the OS shows disk space). */
export function sizeLabel(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

function PartsList({ plan }: { plan: SetupPlan }) {
  return (
    <ul className="setup-parts" aria-label="What setup installs">
      {plan.parts.map((p) => (
        <li key={p.id} className={`setup-part ${p.done ? "is-done" : ""}`}>
          <span className="setup-part__mark" aria-hidden="true">
            {p.done ? <Check size={13} strokeWidth={2.6} /> : <Download size={13} strokeWidth={2.2} />}
          </span>
          <span className="setup-part__label">{p.label}</span>
          <span className="setup-part__size num">{p.done ? "Installed" : sizeLabel(p.bytes)}</span>
        </li>
      ))}
    </ul>
  );
}

function SpaceLine({ plan }: { plan: SetupPlan }) {
  const short = plan.freeBytes != null && plan.neededBytes > 0 && plan.freeBytes < plan.neededBytes;
  return (
    <p className={`setup-space ${short ? "is-bad" : ""}`}>
      Download about <strong className="num">{sizeLabel(plan.downloadBytes)}</strong>
      {plan.neededBytes > 0 && (
        <>
          {" "}
          · needs about <strong className="num">{sizeLabel(plan.neededBytes)}</strong> free while installing
        </>
      )}
      {plan.freeBytes != null && (
        <>
          {" "}
          · <span className="num">{sizeLabel(plan.freeBytes)}</span> free now
        </>
      )}
    </p>
  );
}

function deviceWords(plan: SetupPlan): string {
  return plan.platform === "macos"
    ? "Runs on your Mac's Apple Silicon graphics, entirely on this Mac."
    : "Uses your NVIDIA graphics card if you have one, otherwise the processor (slower). Everything runs on this PC.";
}

function Intro({ plan, onStart }: { plan: SetupPlan; onStart: () => void }) {
  if (!plan.supported) {
    return (
      <div className="setup-card">
        <div className="setup-alert" role="status">
          <CircleAlert size={18} strokeWidth={2.1} aria-hidden="true" />
          <p>
            Automatic speech setup isn't available on this computer. Wren's speech engine needs Windows on an x64 PC, or a Mac
            with Apple Silicon.
          </p>
        </div>
      </div>
    );
  }
  return (
    <div className="setup-card">
      <PartsList plan={plan} />
      <SpaceLine plan={plan} />
      <ul className="setup-notes">
        <li>
          <Cpu size={14} strokeWidth={2} aria-hidden="true" />
          {deviceWords(plan)}
        </li>
        <li>
          <ShieldCheck size={14} strokeWidth={2} aria-hidden="true" />
          This one download is the only time Wren goes online for speech. What you say never leaves this computer.
        </li>
        <li>
          <Info size={14} strokeWidth={2} aria-hidden="true" />
          No admin rights needed. It installs into <span className="setup-path">{plan.location}</span> and can be paused at any time.
        </li>
      </ul>
      <div className="setup-actions">
        <Button variant="primary" icon={<Download size={15} strokeWidth={2.2} />} onClick={onStart}>
          Download and set up
        </Button>
      </div>
    </div>
  );
}

function ProgressBar({ fraction, label }: { fraction: number; label: string }) {
  const pct = Math.round(Math.min(1, Math.max(0, fraction)) * 100);
  return (
    <div className="setup-bar" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
      <span className="setup-bar__fill" style={{ width: `${pct}%` }} />
    </div>
  );
}

function Running({ status, onPause }: { status: Extract<SetupStatus, { state: "running" }>; onPause: () => void }) {
  const pct = Math.round(status.fraction * 100);
  return (
    <div className="setup-card" aria-live="polite">
      <div className="setup-step">
        <span className="setup-step__count">
          Step {status.stepIndex} of {status.stepCount}
        </span>
        <span className="setup-step__label">{status.stepLabel}</span>
        <span className="setup-step__pct num">{pct}%</span>
      </div>
      <ProgressBar fraction={status.fraction} label="Speech setup progress" />
      <p className="setup-detail">{status.detail || " "}</p>
      <PartsList plan={status.plan} />
      <div className="setup-actions">
        <Button variant="secondary" icon={<Pause size={14} strokeWidth={2.2} />} onClick={onPause}>
          Pause
        </Button>
        <span className="setup-hint">You can close this window: setup carries on in the background.</span>
      </div>
    </div>
  );
}

function Stopped({ status, onRetry }: { status: Extract<SetupStatus, { state: "failed" }>; onRetry: () => void }) {
  return (
    <div className="setup-card">
      <div className={`setup-alert ${status.paused ? "" : "is-bad"}`} role={status.paused ? "status" : "alert"}>
        {status.paused ? <Pause size={18} strokeWidth={2.1} aria-hidden="true" /> : <CircleAlert size={18} strokeWidth={2.1} aria-hidden="true" />}
        <p>{status.message}</p>
      </div>
      <PartsList plan={status.plan} />
      {status.step !== "engine" && <SpaceLine plan={status.plan} />}
      <div className="setup-actions">
        <Button
          variant="primary"
          icon={status.paused ? <Play size={14} strokeWidth={2.2} /> : <RotateCw size={14} strokeWidth={2.2} />}
          onClick={onRetry}
        >
          {status.paused ? "Continue setup" : "Try again"}
        </Button>
        <span className="setup-hint">
          {status.paused ? "Finished parts are kept." : "It carries on from where it stopped."} Details:{" "}
          <span className="setup-path">{status.plan.logPath}</span>
        </span>
      </div>
    </div>
  );
}

function Warming({ engine }: { engine: EngineStatus }) {
  const label = engine.state === "warming" ? "Loading the model and transcribing a short test clip" : "Starting the speech engine";
  return (
    <div className="setup-card" aria-live="polite">
      <div className="setup-step">
        <span className="setup-step__label">{label}…</span>
      </div>
      <ProgressBar fraction={0.985} label="Speech setup progress" />
      <p className="setup-detail">Everything is downloaded. The first start takes a little longer.</p>
    </div>
  );
}

function Ready({
  status,
  isMac,
  micDevice,
  onFinish,
}: {
  status: Extract<SetupStatus, { state: "done" }>;
  isMac: boolean;
  micDevice: string | null;
  onFinish: () => void;
}) {
  const mic = useMicLevel(true, micDevice);
  const [axOk, setAxOk] = useState<boolean | null>(isMac ? null : true);
  const [tried, setTried] = useState("");

  useEffect(() => {
    if (!isMac) return;
    const check = () => void accessibilityStatus().then(setAxOk).catch(() => setAxOk(true));
    check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, [isMac]);

  const where =
    status.device === "cpu" ? "on the processor (slower than a graphics card)" : status.device === "mlx" ? "on Apple Silicon" : "on your graphics card";
  const lit = mic.state === "on" ? Math.round(mic.level * METER_SEGMENTS) : 0;
  return (
    <div className="setup-card setup-card--ready">
      <div className="setup-ready">
        <span className="setup-ready__icon" aria-hidden="true">
          <Check size={20} strokeWidth={2.6} />
        </span>
        <div>
          <h2 className="setup-ready__title">Speech is ready</h2>
          <p className="setup-ready__text">
            The speech engine ({status.model}) started {where} and passed its test clip.
          </p>
        </div>
      </div>

      <ol className="setup-checks">
        <li className="setup-check">
          <span className="tinted-icon" aria-hidden="true">
            <Mic size={15} strokeWidth={2} />
          </span>
          <div className="setup-check__body">
            <div className="setup-check__title">Microphone</div>
            {mic.state === "error" ? (
              <div className="setup-check__row">
                <span className="is-bad">{mic.message}</span>
                <Button size="sm" variant="secondary" onClick={() => void popupOpenMicSettings()}>
                  Open microphone settings
                </Button>
              </div>
            ) : (
              <div className="setup-check__row">
                <div className="meter" aria-hidden="true">
                  {Array.from({ length: METER_SEGMENTS }, (_, i) => (
                    <span key={i} className={`meter__seg ${i < lit ? "is-lit" : ""} ${i >= METER_SEGMENTS - 3 ? "is-hot" : ""}`} />
                  ))}
                </div>
                <span className="setup-check__note">{mic.state === "on" ? `Say something: the bars should move. (${mic.device ?? "default microphone"})` : "Opening the microphone…"}</span>
              </div>
            )}
          </div>
        </li>
        {isMac && (
          <li className="setup-check">
            <span className="tinted-icon" aria-hidden="true">
              <ShieldCheck size={15} strokeWidth={2} />
            </span>
            <div className="setup-check__body">
              <div className="setup-check__title">Keyboard shortcut permission</div>
              <div className="setup-check__row">
                {axOk ? (
                  <span className="setup-check__note">Allowed in Privacy & Security → Accessibility.</span>
                ) : (
                  <>
                    <span className="setup-check__note">Wren needs Accessibility access to see the shortcut and paste.</span>
                    <Button size="sm" variant="secondary" onClick={() => void openAccessibilitySettings().then(() => accessibilityStatus().then(setAxOk))}>
                      Allow in System Settings
                    </Button>
                  </>
                )}
              </div>
            </div>
          </li>
        )}
        <li className="setup-check">
          <span className="tinted-icon" aria-hidden="true">
            <Keyboard size={15} strokeWidth={2} />
          </span>
          <div className="setup-check__body">
            <div className="setup-check__title">Try it</div>
            <div className="setup-try">
              <ChordKeys />
              <p className="setup-check__note">
                Click in the box, then hold {isMac ? "fn and Control" : "the left Ctrl and Windows keys"} while you say a sentence. Let go and
                Wren types it here.
              </p>
            </div>
            <textarea
              className="input setup-try__box"
              rows={2}
              value={tried}
              onChange={(e) => setTried(e.target.value)}
              placeholder="Your words appear here"
              aria-label="Try dictating here"
            />
          </div>
        </li>
      </ol>
      <div className="setup-actions">
        <Button variant="primary" onClick={onFinish}>
          Start using Wren
        </Button>
      </div>
    </div>
  );
}

export default function Setup({
  setup,
  engine,
  isMac,
  micDevice,
  onStart,
  onPause,
  onFinish,
}: {
  setup: SetupStatus;
  engine: EngineStatus;
  isMac: boolean;
  micDevice: string | null;
  onStart: () => void;
  onPause: () => void;
  onFinish: () => void;
}) {
  const ready = setup.state === "done";
  return (
    <div className="page page--setup">
      <header className="home-heading">
        <h1 className="page__title">{ready ? "You're all set" : "Set up speech recognition"}</h1>
        {!ready && (
          <p className="page__lede">
            Wren turns your voice into text on this {isMac ? "Mac" : "PC"}. Before your first dictation it needs its speech engine and
            models — a one-time download.
          </p>
        )}
      </header>
      {setup.state === "needed" && <Intro plan={setup.plan} onStart={onStart} />}
      {setup.state === "running" && <Running status={setup} onPause={onPause} />}
      {setup.state === "failed" && <Stopped status={setup} onRetry={onStart} />}
      {setup.state === "starting" && <Warming engine={engine} />}
      {setup.state === "done" && <Ready status={setup} isMac={isMac} micDevice={micDevice} onFinish={onFinish} />}
      {setup.state === "notNeeded" && (
        <div className="setup-card">
          <p className="setup-detail">Speech recognition is already set up on this {isMac ? "Mac" : "PC"}.</p>
          <div className="setup-actions">
            <Button variant="primary" onClick={onFinish}>
              Go to Home
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

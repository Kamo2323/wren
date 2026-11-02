// Settings (spec §11): microphone + live level test, dictation basics, correction watcher, AI clean-up,
// engine, history, other dictation apps, version. Every switch saves at once through `save_settings`.
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type { AiCleanupStatus, EngineStatus, Settings as SettingsData, WatcherStatus } from "../lib/types";
import {
  aiCleanupPullModel,
  aiCleanupStatus,
  appVersion,
  audioDevices,
  engineRestart,
  getSettings,
  historyDeleteAll,
  saveSettings,
  setAudioDevice,
  watcherStatus,
} from "../lib/api";
import { formatNumber, joinNames, retentionLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import { useMicLevel } from "../lib/useMicLevel";
import { engineWords } from "../components/Sidebar";
import { Toggle } from "../components/Toggle";
import { Button } from "../components/Button";
import { useToast } from "../components/Toast";
import { Check, CircleAlert, Cpu, Download, Eye, History, Info, Keyboard, Mic, RotateCw, ShieldCheck, Trash2, TriangleAlert, WandSparkles, WrenTile } from "../components/Icon";

// ---------------------------------------------------------------- layout pieces

function Group({ id, title, icon, children }: { id: string; title: string; icon: ReactNode; children: ReactNode }) {
  return (
    <section className="setgroup" aria-labelledby={`set-${id}`}>
      <h2 className="setgroup__title" id={`set-${id}`}>
        <span className="setgroup__icon">{icon}</span>
        {title}
      </h2>
      <div className="setcard">{children}</div>
    </section>
  );
}

function SetRow({
  title,
  desc,
  control,
  children,
  htmlFor,
  dot,
}: {
  title: string;
  desc?: ReactNode;
  control?: ReactNode;
  children?: ReactNode;
  htmlFor?: string;
  dot?: string;
}) {
  return (
    <div className="setrow">
      <div className="setrow__main">
        <div className="setrow__text">
          {htmlFor ? (
            <label className="setrow__title" htmlFor={htmlFor}>
              {title}
            </label>
          ) : (
            <div className="setrow__title">
              {dot && <span className={`dot dot--${dot}`} aria-hidden="true" />}
              {title}
            </div>
          )}
          {desc && <div className="setrow__desc">{desc}</div>}
        </div>
        {control && <div className="setrow__control">{control}</div>}
      </div>
      {children}
    </div>
  );
}

function Status({ tone, children }: { tone: "ok" | "warn" | "bad" | "info"; children: ReactNode }) {
  const icon =
    tone === "ok" ? <Check size={15} strokeWidth={2.6} /> : tone === "info" ? <Info size={15} strokeWidth={2.2} /> : <CircleAlert size={15} strokeWidth={2.2} />;
  return (
    <div className={`status status--${tone}`}>
      <span className="status__icon">{icon}</span>
      <span>{children}</span>
    </div>
  );
}

// ---------------------------------------------------------------- microphone

const METER_SEGMENTS = 28;
const TEST_MS = 15_000;

function MicSection({ settings, update }: { settings: SettingsData; update: (patch: Partial<SettingsData>) => Promise<void> }) {
  const [devices, setDevices] = useState<string[] | null>(null);
  const [testing, setTesting] = useState(false);
  const toast = useToast();
  const mic = useMicLevel(testing, settings.micDevice);

  useEffect(() => {
    void audioDevices()
      .then(setDevices)
      .catch(() => setDevices([]));
  }, []);

  useEffect(() => {
    if (!testing) return;
    const t = window.setTimeout(() => setTesting(false), TEST_MS);
    return () => window.clearTimeout(t);
  }, [testing]);

  async function pick(value: string) {
    const name = value === "" ? null : value;
    try {
      await setAudioDevice(name);
      await update({ micDevice: name });
    } catch {
      toast.show("Couldn't switch microphone.", { tone: "danger" });
    }
  }

  const list = devices ?? [];
  const missing = settings.micDevice != null && devices != null && !list.includes(settings.micDevice);
  const lit = mic.state === "on" ? Math.round(mic.level * METER_SEGMENTS) : 0;

  return (
    <Group id="mic" title="Microphone" icon={<Mic size={17} strokeWidth={2} />}>
      <SetRow
        title="Microphone"
        htmlFor="mic-select"
        desc={missing ? "That microphone isn't plugged in. Wren is using the Windows default for now." : "Wren follows the Windows default unless you pick one here."}
        control={
          <select id="mic-select" className="select" value={settings.micDevice ?? ""} onChange={(e) => void pick(e.target.value)}>
            <option value="">Windows default</option>
            {missing && <option value={settings.micDevice ?? ""}>{settings.micDevice} (not found)</option>}
            {list.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        }
      >
        <div className="mictest">
          <Button size="sm" variant={testing ? "primary" : "secondary"} icon={<Mic size={15} strokeWidth={2.2} />} onClick={() => setTesting((t) => !t)} aria-pressed={testing}>
            {testing ? "Stop test" : "Test microphone"}
          </Button>
          <div className={`meter ${testing ? "is-on" : ""}`} role="meter" aria-label="Microphone level" aria-valuemin={0} aria-valuemax={100} aria-valuenow={mic.state === "on" ? Math.round(mic.level * 100) : 0}>
            {Array.from({ length: METER_SEGMENTS }, (_, i) => (
              <span key={i} className={`meter__seg ${i < lit ? "is-lit" : ""} ${i >= METER_SEGMENTS - 4 ? "is-hot" : ""}`} />
            ))}
          </div>
          <span className="mictest__note">
            {mic.state === "error" ? <span className="is-bad">{mic.message}</span> : testing ? "Say something. The bar should move as you talk." : "Check Wren can hear you."}
          </span>
        </div>
      </SetRow>
      <SetRow
        title="Keep the microphone open"
        desc="Catches your first word if it gets cut off. Windows will show the microphone as in use all the time."
        control={<Toggle checked={settings.alwaysOpenMic} onChange={(v) => void update({ alwaysOpenMic: v })} label="Keep the microphone open" />}
      />
    </Group>
  );
}

// ---------------------------------------------------------------- dictation basics

function TypingSpeed({ value, fallback, onSave }: { value: number; fallback: number; onSave: (wpm: number) => Promise<void> }) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  const n = Number(text);
  const valid = Number.isInteger(n) && n >= 10 && n <= 200;

  function commit() {
    if (!valid) {
      setText(String(value));
      return;
    }
    if (n !== value) void onSave(n);
  }

  return (
    <div className="wpm">
      <div className={`wpm__box ${valid ? "" : "is-invalid"}`}>
        <input
          id="typing-wpm"
          className="wpm__input num"
          inputMode="numeric"
          value={text}
          maxLength={3}
          onChange={(e) => setText(e.target.value.replace(/[^0-9]/g, ""))}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            if (e.key === "Escape") setText(String(value));
          }}
          aria-describedby="typing-wpm-desc"
        />
        <span className="wpm__unit">wpm</span>
      </div>
      {value !== fallback && (
        <button type="button" className="linkbtn" onClick={() => void onSave(fallback)}>
          Reset to {fallback}
        </button>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- watcher + AI

function WatcherSection({ settings, update }: { settings: SettingsData; update: (patch: Partial<SettingsData>) => Promise<void> }) {
  const [status, setStatus] = useState<WatcherStatus | null>(null);
  useEffect(() => {
    void watcherStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [settings.watcherEnabled]);

  const automatic = settings.watcherEnabled === null;
  const on = settings.watcherEnabled ?? status?.enabled ?? false;

  return (
    <Group id="learning" title="Learning" icon={<Eye size={17} strokeWidth={2} />}>
      <SetRow
        title="Learn from my corrections"
        desc="After Wren pastes, it reads the text box for a little while. If you change a word it got wrong, Wren learns it. It never reads terminals."
        control={<Toggle checked={on} onChange={(v) => void update({ watcherEnabled: v })} label="Learn from my corrections" />}
      >
        <div className="setrow__foot">
          {status === null ? (
            <Status tone="info">The check of the Claude app hasn't run yet.</Status>
          ) : (
            <Status tone={status.enabled ? "ok" : "warn"}>
              {status.reason ?? (status.enabled ? "The Claude app's text box can be read quickly." : "Off: the Claude app's text box couldn't be read quickly.")}
            </Status>
          )}
          {automatic ? (
            <span className="setrow__aside">Set automatically from that check</span>
          ) : (
            <button type="button" className="linkbtn" onClick={() => void update({ watcherEnabled: null })}>
              Go back to automatic
            </button>
          )}
        </div>
      </SetRow>
    </Group>
  );
}

function AiSection({ settings, update }: { settings: SettingsData; update: (patch: Partial<SettingsData>) => Promise<void> }) {
  const [status, setStatus] = useState<AiCleanupStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [pulling, setPulling] = useState(false);
  const toast = useToast();

  const check = useCallback(async () => {
    setChecking(true);
    const s = await aiCleanupStatus().catch(() => null);
    setStatus(s);
    setChecking(false);
  }, []);

  useEffect(() => {
    void check();
  }, [check]);

  async function pull() {
    setPulling(true);
    try {
      await aiCleanupPullModel();
      toast.show("Model downloaded", { tone: "success", icon: <Check size={16} strokeWidth={2.6} /> });
    } catch {
      toast.show("The download didn't finish. Try again.", { tone: "danger" });
    } finally {
      setPulling(false);
      void check();
    }
  }

  const model = status?.model || settings.aiCleanup.model;
  const ready = status?.ollamaReachable && status.modelPresent;

  return (
    <Group id="ai" title="AI clean-up" icon={<WandSparkles size={17} strokeWidth={2} />}>
      <SetRow
        title="Tidy text with a local AI model"
        desc="Fixes punctuation, capitals and obvious mis-hearings using Ollama on this PC. Nothing leaves your computer. If it's slow, Wren pastes the plain text instead."
        control={<Toggle checked={settings.aiCleanup.enabled} onChange={(v) => void update({ aiCleanup: { ...settings.aiCleanup, enabled: v } })} label="Tidy text with a local AI model" />}
      >
        <div className="setrow__foot">
          {status === null ? (
            <Status tone="info">{checking ? "Checking Ollama…" : "Couldn't check Ollama."}</Status>
          ) : !status.ollamaReachable ? (
            <Status tone={settings.aiCleanup.enabled ? "bad" : "warn"}>Ollama isn't running. Start Ollama, then check again.</Status>
          ) : !status.modelPresent ? (
            <Status tone="warn">
              Ollama is running, but the model <code>{model}</code> isn't downloaded yet.
            </Status>
          ) : (
            <Status tone="ok">
              Ready: Ollama is running and <code>{model}</code> is downloaded.
            </Status>
          )}
          <div className="setrow__foot-actions">
            {status?.ollamaReachable && !status.modelPresent && (
              <Button size="sm" variant="primary" icon={pulling ? <span className="spinner" aria-hidden="true" /> : <Download size={15} strokeWidth={2.2} />} disabled={pulling} onClick={() => void pull()}>
                {pulling ? "Downloading…" : `Download ${model}`}
              </Button>
            )}
            {!ready && !pulling && (
              <Button size="sm" variant="ghost" icon={<RotateCw size={15} strokeWidth={2.2} />} disabled={checking} onClick={() => void check()}>
                Check again
              </Button>
            )}
          </div>
        </div>
        {pulling && <p className="setrow__note">This can take a few minutes. You can keep using Wren meanwhile.</p>}
      </SetRow>
    </Group>
  );
}

// ---------------------------------------------------------------- engine

function EngineSection({ engine, settings }: { engine: EngineStatus; settings: SettingsData }) {
  const [restarting, setRestarting] = useState(false);
  const toast = useToast();
  const words = engineWords(engine);
  const ready = engine.state === "ready" ? engine : null;

  async function restart() {
    setRestarting(true);
    try {
      await engineRestart();
      toast.show("Restarting the engine…");
    } catch {
      toast.show("Couldn't restart the engine.", { tone: "danger" });
    } finally {
      window.setTimeout(() => setRestarting(false), 1500);
    }
  }

  return (
    <Group id="engine" title="Speech engine" icon={<Cpu size={17} strokeWidth={2} />}>
      <SetRow
        title={words.text}
        dot={words.tone}
        desc={engine.state === "failed" ? <span className="is-bad">{engine.reason || "The engine stopped."}</span> : "Turns your voice into text, on this PC."}
        control={
          <Button size="sm" variant="secondary" icon={<RotateCw size={15} strokeWidth={2.2} />} disabled={restarting} onClick={() => void restart()}>
            Restart engine
          </Button>
        }
      >
        <dl className="specs">
          <div className="spec">
            <dt>Model</dt>
            <dd>{ready?.model || settings.engine.model}</dd>
          </div>
          <div className="spec">
            <dt>Runs on</dt>
            <dd>{ready ? (ready.device === "cpu" ? "Processor (slower)" : "Graphics card") : "–"}</dd>
          </div>
          <div className="spec">
            <dt>Graphics memory</dt>
            <dd className="num">{ready && ready.vramMb > 0 ? `${(ready.vramMb / 1024).toFixed(1)} GB` : "–"}</dd>
          </div>
        </dl>
      </SetRow>
    </Group>
  );
}

// ---------------------------------------------------------------- history

function HistorySection() {
  const constants = useConstants();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const toast = useToast();
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (confirming) cancelRef.current?.focus();
  }, [confirming]);

  async function deleteAll() {
    setBusy(true);
    try {
      const n = await historyDeleteAll();
      toast.show(n === 0 ? "History was already empty" : `Deleted ${formatNumber(n)} ${n === 1 ? "dictation" : "dictations"}`, {
        tone: "success",
        icon: <Check size={16} strokeWidth={2.6} />,
      });
      setConfirming(false);
    } catch {
      toast.show("Couldn't delete history.", { tone: "danger" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Group id="history" title="History" icon={<History size={17} strokeWidth={2} />}>
      <SetRow
        title="Delete all history now"
        desc={`Wren deletes dictations and their recordings after ${retentionLabel(constants.retentionHours)} anyway. This does it now. Your stats and dictionary stay.`}
        control={
          !confirming && (
            <Button size="sm" variant="secondary" className="btn--danger-outline" icon={<Trash2 size={15} strokeWidth={2.1} />} onClick={() => setConfirming(true)}>
              Delete all…
            </Button>
          )
        }
      >
        {confirming && (
          <div className="confirm" role="alertdialog" aria-label="Delete all history?">
            <span className="confirm__text">
              <TriangleAlert size={17} strokeWidth={2.1} /> Delete every dictation and recording? This can't be undone.
            </span>
            <div className="confirm__actions">
              <Button ref={cancelRef} size="sm" variant="ghost" onClick={() => setConfirming(false)}>
                Cancel
              </Button>
              <Button size="sm" variant="danger" disabled={busy} onClick={() => void deleteAll()}>
                Delete all
              </Button>
            </div>
          </div>
        )}
      </SetRow>
    </Group>
  );
}

// ---------------------------------------------------------------- page

export default function Settings({
  engine,
  others,
  onSaved,
}: {
  engine: EngineStatus;
  others: string[];
  onSaved: (s: SettingsData) => void;
}) {
  const constants = useConstants();
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [version, setVersion] = useState("");
  const toast = useToast();
  const latest = useRef<SettingsData | null>(null);

  useEffect(() => {
    void getSettings()
      .then((s) => {
        latest.current = s;
        setSettings(s);
      })
      .catch(() => toast.show("Couldn't load settings.", { tone: "danger" }));
    void appVersion()
      .then(setVersion)
      .catch(() => {});
  }, [toast]);

  /** Apply a change at once, save it, and put it back if the save fails. */
  const update = useCallback(
    async (patch: Partial<SettingsData>) => {
      const before = latest.current;
      if (!before) return;
      const next = { ...before, ...patch };
      latest.current = next;
      setSettings(next);
      try {
        await saveSettings(next);
        onSaved(next);
      } catch {
        latest.current = before;
        setSettings(before);
        toast.show("Couldn't save that setting.", { tone: "danger" });
      }
    },
    [onSaved, toast],
  );

  return (
    <div className="page page--settings">
      <header className="page-head">
        <div>
          <h1 className="page__title">Settings</h1>
          <p className="page__lede">Everything saves as you change it.</p>
        </div>
      </header>

      {settings && (
        <div className="setgroups">
          <MicSection settings={settings} update={update} />

          <Group id="dictation" title="Dictation" icon={<Keyboard size={17} strokeWidth={2} />}>
            <SetRow
              title="Your typing speed"
              htmlFor="typing-wpm"
              desc={<span id="typing-wpm-desc">Used to work out time saved. {constants.typingWpmDefault} wpm is a typical typing speed.</span>}
              control={<TypingSpeed value={settings.typingWpm} fallback={constants.typingWpmDefault} onSave={(wpm) => update({ typingWpm: wpm })} />}
            />
            <SetRow
              title="Sounds"
              desc="A soft click when Wren starts and stops listening."
              control={<Toggle checked={settings.sounds} onChange={(v) => void update({ sounds: v })} label="Sounds" />}
            />
            <SetRow
              title="Start with Windows"
              desc="Off by default. It's safe to turn on once Willow and Glaido no longer start with Windows, so only one app listens for the keys."
              control={<Toggle checked={settings.startWithWindows} onChange={(v) => void update({ startWithWindows: v })} label="Start with Windows" />}
            />
          </Group>

          <WatcherSection settings={settings} update={update} />
          <AiSection settings={settings} update={update} />
          <EngineSection engine={engine} settings={settings} />
          <HistorySection />

          <Group
            id="others"
            title="Other dictation apps"
            icon={others.length ? <TriangleAlert size={17} strokeWidth={2} /> : <ShieldCheck size={17} strokeWidth={2} />}
          >
            <SetRow
              title={others.length ? `${joinNames(others)} ${others.length === 1 ? "is" : "are"} running` : "No other dictation apps running"}
              desc={
                others.length
                  ? "If they use the same keys, you'll get two pastes. Close them, or turn off their shortcut. Wren never closes or changes them."
                  : "Wren checks for Willow and Glaido when it starts. Running two at once can paste everything twice."
              }
            >
              {others.length > 0 && (
                <div className="setrow__foot">
                  <Status tone="warn">Double pastes are likely until {others.length === 1 ? "it's" : "they're"} closed.</Status>
                </div>
              )}
            </SetRow>
          </Group>

          <footer className="about">
            <WrenTile size={40} />
            <div>
              <div className="about__name">
                Wren <span className="about__version num">{version ? `v${version}` : ""}</span>
              </div>
              <div className="about__line">Free and local. Your voice never leaves this PC.</div>
            </div>
          </footer>
        </div>
      )}
    </div>
  );
}

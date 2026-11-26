// Settings: quick section index, Appearance (Dark / Light / System), Microphone + live VU test,
// Dictation, Learning watcher, AI clean-up, Speech engine, History, Other dictation apps, Updates.
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type { EngineStatus, Settings as SettingsData, UpdateStatus, WatcherStatus } from "../lib/types";
import { toSettingsTheme, type ThemeChoice } from "../lib/theme";
import {
  appVersion,
  audioDevices,
  engineRestart,
  getSettings,
  historyDeleteAll,
  saveSettings,
  setAudioDevice,
  watcherStatus,
} from "../lib/api";
import {
  CLAUDE_KEY_EVENT,
  claudeKeyRemove,
  claudeKeySave,
  claudeKeyStatus,
  claudeSpendMonth,
  errorText,
  openSystemAudioSettings,
  systemAudioPermission,
} from "../lib/meetings";
import { formatBytes, formatNumber, joinNames, retentionLabel, whenLabel } from "../lib/format";
import { spendLine, type ClaudeSpend } from "../lib/meetingText";
import { useConstants } from "../lib/useConstants";
import { useMicLevel } from "../lib/useMicLevel";
import { LoadError } from "../components/Empty";
import { engineWords } from "../components/Sidebar";
import { Toggle } from "../components/Toggle";
import { Button } from "../components/Button";
import { useToast } from "../components/Toast";
import { FIX_PERMISSIONS_HINT, FixPermissionsButton } from "../components/FixPermissions";
import {
  Check,
  CircleAlert,
  Cpu,
  Download,
  Eye,
  History,
  Info,
  Keyboard,
  Mic,
  Monitor,
  Moon,
  RotateCw,
  Wrench,
  ShieldCheck,
  Sun,
  Trash2,
  TriangleAlert,
  Users,
  WandSparkles,
  WrenTile,
} from "../components/Icon";

type Tone = "violet" | "teal" | "amber";

const SECTIONS: Array<{ id: string; label: string }> = [
  { id: "appearance", label: "Appearance" },
  { id: "mic", label: "Microphone" },
  { id: "dictation", label: "Dictation" },
  { id: "learning", label: "Learning" },
  { id: "ai", label: "AI clean-up" },
  { id: "meetings", label: "Meeting notes" },
  { id: "engine", label: "Speech engine" },
  { id: "history", label: "History" },
  { id: "updates", label: "Updates" },
];

// ---------------------------------------------------------------- layout pieces

function Group({ id, title, icon, tone, children }: { id: string; title: string; icon: ReactNode; tone: Tone; children: ReactNode }) {
  return (
    <section className="setgroup" data-tone={tone} aria-labelledby={`set-${id}`}>
      <h2 className="setgroup__title" id={`set-${id}`} tabIndex={-1}>
        <span className="tinted-icon" aria-hidden="true">
          {icon}
        </span>
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
    tone === "ok" ? <Check size={14} strokeWidth={2.5} /> : tone === "info" ? <Info size={14} strokeWidth={2.1} /> : <CircleAlert size={14} strokeWidth={2.1} />;
  return (
    <div className={`status status--${tone}`}>
      <span className="status__icon">{icon}</span>
      <span>{children}</span>
    </div>
  );
}

// ---------------------------------------------------------------- appearance

function AppearanceSection({
  choice,
  onSelect,
  isMac,
}: {
  choice: ThemeChoice;
  onSelect: (next: ThemeChoice) => void;
  isMac: boolean;
}) {
  return (
    <Group id="appearance" title="Appearance" icon={<Sun size={15} strokeWidth={2} />} tone="amber">
      <SetRow
        title="Colour theme"
        desc={`Choose Dark, Light, or follow your ${isMac ? "macOS" : "Windows"} theme automatically.`}
        control={
          <div className="segments theme-switch" role="group" aria-label="Colour theme">
            <button
              type="button"
              className="segments__btn"
              data-theme-opt="dark"
              aria-pressed={choice === "dark"}
              onClick={() => onSelect("dark")}
            >
              <Moon size={13} strokeWidth={2} aria-hidden="true" />
              Dark
            </button>
            <button
              type="button"
              className="segments__btn"
              data-theme-opt="light"
              aria-pressed={choice === "light"}
              onClick={() => onSelect("light")}
            >
              <Sun size={13} strokeWidth={2} aria-hidden="true" />
              Light
            </button>
            <button
              type="button"
              className="segments__btn"
              data-theme-opt="system"
              aria-pressed={choice === "system"}
              onClick={() => onSelect("system")}
            >
              <Monitor size={13} strokeWidth={2} aria-hidden="true" />
              System
            </button>
          </div>
        }
      />
    </Group>
  );
}

// ---------------------------------------------------------------- microphone

const METER_SEGMENTS = 28;
const TEST_MS = 15_000;

function MicSection({
  settings,
  update,
  isMac,
}: {
  settings: SettingsData;
  update: (patch: Partial<SettingsData>) => Promise<void>;
  isMac: boolean;
}) {
  const osName = isMac ? "macOS" : "Windows";
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
    <Group id="mic" title="Microphone" icon={<Mic size={15} strokeWidth={2} />} tone="teal">
      <SetRow
        title="Which microphone"
        htmlFor="mic-select"
        desc={missing ? `That microphone isn't plugged in. Wren is using the ${osName} default for now.` : `Wren follows the ${osName} default unless you pick one here.`}
        control={
          <select id="mic-select" className="select" value={settings.micDevice ?? ""} onChange={(e) => void pick(e.target.value)}>
            <option value="">{osName} default</option>
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
          <Button size="sm" variant={testing ? "primary" : "secondary"} icon={<Mic size={14} strokeWidth={2.2} />} onClick={() => setTesting((t) => !t)} aria-pressed={testing}>
            {testing ? "Stop test" : "Test microphone"}
          </Button>
          <div className={`meter ${testing ? "is-on" : ""}`} role="meter" aria-label="Microphone level" aria-valuemin={0} aria-valuemax={100} aria-valuenow={mic.state === "on" ? Math.round(mic.level * 100) : 0}>
            {Array.from({ length: METER_SEGMENTS }, (_, i) => (
              <span key={i} className={`meter__seg ${i < lit ? "is-lit" : ""} ${i >= METER_SEGMENTS - 4 ? "is-hot" : ""}`} />
            ))}
          </div>
          <span className="mictest__note">
            {mic.state === "error" ? (
              <span className="is-bad">{mic.message}</span>
            ) : mic.state === "on" && mic.fellBack ? (
              <span className="is-bad">
                Couldn't open “{settings.micDevice}”. Testing “{mic.device ?? `the ${osName} default`}” instead.
              </span>
            ) : mic.state === "on" ? (
              <>
                Listening to <strong className="mictest__device">{mic.device ?? `the ${osName} default`}</strong>. Say something; the bar should move.
              </>
            ) : (
              "Check Wren can hear you."
            )}
          </span>
        </div>
      </SetRow>
      <SetRow
        title="Keep the microphone open"
        desc={`Catches your first word if it gets cut off. ${osName} will show the microphone as in use all the time.`}
        control={<Toggle checked={settings.alwaysOpenMic} onChange={(v) => void update({ alwaysOpenMic: v })} label="Keep the microphone open" />}
      />
    </Group>
  );
}

// ---------------------------------------------------------------- dictation basics

const WPM_MIN = 10;
const WPM_MAX = 200;

function TypingSpeed({ value, fallback, onSave }: { value: number; fallback: number; onSave: (wpm: number) => Promise<void> }) {
  const [text, setText] = useState(String(value));
  const [rejected, setRejected] = useState(false);
  useEffect(() => setText(String(value)), [value]);

  function commit() {
    const n = Number(text);
    if (!(text !== "" && Number.isInteger(n) && n >= WPM_MIN && n <= WPM_MAX)) {
      if (text !== String(value)) setRejected(true);
      setText(String(value));
      return;
    }
    setRejected(false);
    if (n !== value) void onSave(n);
  }

  return (
    <div className="wpm">
      {rejected && (
        <span id="typing-wpm-error" className="wpm__error" role="alert">
          Use {WPM_MIN} to {WPM_MAX}
        </span>
      )}
      <div className={`wpm__box ${rejected ? "is-invalid" : ""}`}>
        <input
          id="typing-wpm"
          className="wpm__input num"
          inputMode="numeric"
          value={text}
          maxLength={3}
          onChange={(e) => {
            setText(e.target.value.replace(/[^0-9]/g, ""));
            setRejected(false);
          }}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            if (e.key === "Escape") {
              setText(String(value));
              setRejected(false);
            }
          }}
          aria-invalid={rejected}
          aria-describedby={rejected ? "typing-wpm-desc typing-wpm-error" : "typing-wpm-desc"}
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

function verdictTone(verdict: string | null): "ok" | "warn" | "info" {
  if (!verdict) return "info";
  return /^(readable|ok|pass|passed|fast|good)$/i.test(verdict.trim()) ? "ok" : "warn";
}

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
    <Group id="learning" title="Learning" icon={<Eye size={15} strokeWidth={2} />} tone="teal">
      <SetRow
        title="Learn from my corrections"
        desc="After Wren pastes, it reads the text box for a little while. If you change a word it got wrong, Wren learns it. It never reads terminals."
        control={<Toggle checked={on} onChange={(v) => void update({ watcherEnabled: v })} label="Learn from my corrections" />}
      >
        <div className="setrow__foot">
          {status === null ? (
            <Status tone="info">The check of the Claude app hasn't run yet.</Status>
          ) : (
            <Status tone={verdictTone(status.verdict)}>
              {status.reason ?? (status.enabled ? "The Claude app's text box can be read quickly." : "Off: the Claude app's text box couldn't be read quickly.")}
            </Status>
          )}
          {automatic ? (
            <span className="setrow__aside">Wren chose this from the check</span>
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

function MeetingNotesSection({
  settings,
  update,
  isMac,
}: {
  settings: SettingsData;
  update: (patch: Partial<SettingsData>) => Promise<void>;
  isMac: boolean;
}) {
  const toast = useToast();
  const [name, setName] = useState(settings.meetingName ?? "");
  const [keySaved, setKeySaved] = useState<boolean | null>(null);
  const [key, setKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [spend, setSpend] = useState<ClaudeSpend | null>(null);
  const [perm, setPerm] = useState<string | null>(null);

  useEffect(() => {
    void claudeKeyStatus()
      .then((k) => setKeySaved(k.saved))
      .catch(() => setKeySaved(false));
    void claudeSpendMonth().then(setSpend).catch(() => {});
    if (!isMac) return;
    const checkPerm = () => void systemAudioPermission().then(setPerm).catch(() => {});
    checkPerm();
    window.addEventListener("focus", checkPerm);
    return () => window.removeEventListener("focus", checkPerm);
  }, [isMac]);

  async function saveKey() {
    setSaving(true);
    setKeyError(null);
    try {
      await claudeKeySave(key.trim());
      setKey("");
      setKeySaved(true);
      window.dispatchEvent(new CustomEvent(CLAUDE_KEY_EVENT));
      toast.show("Claude API key saved");
    } catch (e) {
      setKeyError(errorText(e));
    } finally {
      setSaving(false);
    }
  }

  async function forgetKey() {
    try {
      await claudeKeyRemove();
      setKeySaved(false);
      window.dispatchEvent(new CustomEvent(CLAUDE_KEY_EVENT));
    } catch (e) {
      toast.show(errorText(e));
    }
  }

  const model = settings.claudeModel === "claude-sonnet-5-5" ? "claude-sonnet-5-5" : "claude-opus-5-5";
  return (
    <Group id="meetings" title="Meeting notes" icon={<Users size={15} strokeWidth={2} />} tone="teal">
      <SetRow
        title="Your name"
        desc="Used to greet you on Home, shown on your own lines in meeting transcripts and used in the notes. Left empty, your lines say “Me”."
        htmlFor="meeting-name"
        control={
          <input
            id="meeting-name"
            className="input"
            value={name}
            maxLength={60}
            placeholder="Me"
            onChange={(e) => setName(e.target.value)}
            onBlur={() => {
              const n = name.trim();
              setName(n);
              if (n !== settings.meetingName) void update({ meetingName: n });
            }}
          />
        }
      />
      <SetRow
        title="Claude API key"
        desc={
          <>
            Enhanced notes (and AI clean-up, if you turn it on) are written by Claude and billed to the Anthropic organisation the key belongs to (your API credits).{" "}
            {isMac
              ? "The key is kept in a private file in Wren's folder that only your Mac account can open, never in Wren's settings or logs."
              : "The key is kept in Windows Credential Manager, never in Wren's files."}
          </>
        }
      >
        <div className="setrow__foot">
          {keySaved ? (
            <div className="key-row">
              <span className="key-saved">
                <Status tone="ok">Key saved</Status>
              </span>
              <Button size="sm" variant="ghost" icon={<Trash2 size={13} strokeWidth={2.2} />} onClick={() => void forgetKey()}>
                Forget key
              </Button>
            </div>
          ) : (
            <form
              className="key-row"
              onSubmit={(e) => {
                e.preventDefault();
                if (key.trim()) void saveKey();
              }}
            >
              <input
                id="claude-key"
                className="input key-input"
                type="password"
                autoComplete="off"
                spellCheck={false}
                placeholder="sk-ant-…"
                aria-label="Claude API key"
                value={key}
                onChange={(e) => setKey(e.target.value)}
              />
              <Button type="submit" size="sm" variant="primary" disabled={saving || !key.trim()}>
                {saving ? "Checking…" : "Save key"}
              </Button>
            </form>
          )}
          {keyError && <Status tone="bad">{keyError}</Status>}
        </div>
      </SetRow>
      <SetRow
        title="Model"
        desc="Opus writes the best notes. Sonnet costs about half as much."
        htmlFor="claude-model"
        control={
          <select id="claude-model" className="select" value={model} onChange={(e) => void update({ claudeModel: e.target.value })}>
            <option value="claude-opus-5-5">Claude Opus 5.5</option>
            <option value="claude-sonnet-5-5">Claude Sonnet 5.5</option>
          </select>
        }
      />
      <SetRow
        title="This month"
        desc={spend ? spendLine(spend) : "Checking…"}
      />
      <SetRow
        title="Save call diagnostics"
        desc="For testing who said what. During each call, Wren saves what Teams shows (who is speaking, everyone in the call by name, whether you’re muted) and the transcript lines to a file next to the meeting’s audio. It stays on this computer and is deleted with the audio after 24 hours, or when you delete the meeting. Starts with the next meeting."
        control={
          <Toggle
            checked={settings.saveCallDiagnostics ?? false}
            onChange={(v) => void update({ saveCallDiagnostics: v })}
            label="Save call diagnostics"
          />
        }
      />
      {isMac && (
        <SetRow
          title="System audio recording"
          desc={
            perm === "granted"
              ? "macOS needs this permission so Wren can hear the other people in your Teams calls."
              : `macOS needs this permission so Wren can hear the other people in your Teams calls. ${FIX_PERMISSIONS_HINT}`
          }
          control={
            perm === "granted" ? (
              <Status tone="ok">Allowed</Status>
            ) : (
              <span className="set-actions">
                <Button size="sm" variant="primary" onClick={() => void openSystemAudioSettings()}>
                  Open System Settings
                </Button>
                <FixPermissionsButton onDone={() => void systemAudioPermission().then(setPerm).catch(() => {})} />
              </span>
            )
          }
        />
      )}
    </Group>
  );
}

const AI_MODELS: Array<{ id: string; label: string }> = [
  { id: "claude-haiku-5-5", label: "Claude Haiku 5.5" },
  { id: "claude-sonnet-5-5", label: "Claude Sonnet 5.5" },
];

function AiSection({
  settings,
  update,
  isMac,
}: {
  settings: SettingsData;
  update: (patch: Partial<SettingsData>) => Promise<void>;
  isMac: boolean;
}) {
  const [keySaved, setKeySaved] = useState<boolean | null>(null);

  useEffect(() => {
    const check = () =>
      void claudeKeyStatus()
        .then((k) => setKeySaved(k.saved))
        .catch(() => setKeySaved(false));
    check();
    window.addEventListener(CLAUDE_KEY_EVENT, check);
    window.addEventListener("focus", check);
    return () => {
      window.removeEventListener(CLAUDE_KEY_EVENT, check);
      window.removeEventListener("focus", check);
    };
  }, []);

  const ai = settings.aiCleanup;
  const set = (patch: Partial<SettingsData["aiCleanup"]>) => void update({ aiCleanup: { ...ai, ...patch } });
  const model = AI_MODELS.some((m) => m.id === ai.model) ? ai.model : AI_MODELS[0].id;
  const style = ai.codingStyle === "tidy" ? "tidy" : "rewrite";
  const noKey = keySaved === false;

  function goToKey() {
    const title = document.getElementById("set-meetings");
    title?.scrollIntoView({ behavior: "smooth", block: "start" });
    window.setTimeout(() => (document.getElementById("claude-key") ?? title)?.focus({ preventScroll: true }), 350);
  }

  return (
    <Group id="ai" title="AI clean-up" icon={<WandSparkles size={15} strokeWidth={2} />} tone="violet">
      <SetRow
        title="Tidy text with Claude"
        desc={
          <>
            Claude fixes punctuation, capitals and mis-heard words, and shapes the text for the app you're typing in: email-ready paragraphs,
            casual chat messages, or a clear prompt for coding agents. Only the text is sent to Anthropic, billed to your Claude API key; your
            voice never leaves {isMac ? "this Mac" : "this PC"}. If Claude is slow, Wren pastes the plain text.
          </>
        }
        control={<Toggle checked={ai.enabled && !noKey} disabled={noKey} onChange={(v) => set({ enabled: v })} label="Tidy text with Claude" />}
      >
        {noKey && (
          <div className="setrow__foot">
            <Status tone={ai.enabled ? "bad" : "warn"}>Add your Claude key under Meeting notes to turn this on.</Status>
            <div className="setrow__foot-actions">
              <Button size="sm" variant="ghost" onClick={goToKey}>
                Go to Meeting notes
              </Button>
            </div>
          </div>
        )}
      </SetRow>
      <SetRow
        title="Model"
        desc="Haiku is quick and costs a fraction of a cent a dictation. Sonnet can phrase things a little better and costs about 20 times as much."
        htmlFor="ai-model"
        control={
          <select id="ai-model" className="select" value={model} onChange={(e) => set({ model: e.target.value })}>
            {AI_MODELS.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        }
      />
      <SetRow
        title="In coding apps"
        desc={
          <>
            For Claude, Codex, T3 Code, VS Code, Cursor and terminals. <b>Rewrite</b> turns rambling speech into a clear prompt (ums, repeats
            and false starts go, steps become a list). <b>Tidy</b> keeps your words and only adds punctuation, <code>backticks</code> and lists.
            Saying “slash compact” types <code>/compact</code>.
          </>
        }
        control={
          <div className="segments" role="group" aria-label="Clean-up style in coding apps">
            <button
              type="button"
              className="segments__btn"
              data-style-opt="rewrite"
              aria-pressed={style === "rewrite"}
              onClick={() => set({ codingStyle: "rewrite" })}
            >
              Rewrite
            </button>
            <button
              type="button"
              className="segments__btn"
              data-style-opt="tidy"
              aria-pressed={style === "tidy"}
              onClick={() => set({ codingStyle: "tidy" })}
            >
              Tidy
            </button>
          </div>
        }
      />
    </Group>
  );
}

// ---------------------------------------------------------------- engine

function EngineSection({
  engine,
  settings,
  isMac,
  onRepair,
}: {
  engine: EngineStatus;
  settings: SettingsData;
  isMac: boolean;
  onRepair?: () => void;
}) {
  const [restarting, setRestarting] = useState(false);
  const toast = useToast();
  const words = engineWords(engine);
  const ready = engine.state === "ready" ? engine : null;
  const runsOn = ready
    ? ready.device === "cpu"
      ? "Processor (slower)"
      : ready.device === "mlx"
        ? "Apple Silicon (MLX)"
        : "Graphics card"
    : "–";
  const memoryLabel = ready?.device === "mlx" || isMac ? "Unified memory" : "Graphics memory";
  const memoryValue =
    ready && ready.vramMb > 0
      ? ready.vramMb < 1024
        ? `${ready.vramMb} MB`
        : `${(ready.vramMb / 1024).toFixed(1)} GB`
      : "–";

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
    <Group id="engine" title="Speech engine" icon={<Cpu size={15} strokeWidth={2} />} tone="teal">
      <SetRow
        title={words.text}
        dot={words.tone}
        desc={
          engine.state === "failed" || engine.state === "needsSetup" ? (
            <span className="is-bad">{engine.reason || "The engine stopped."}</span>
          ) : (
            `Turns your voice into text, on ${isMac ? "this Mac" : "this PC"}.`
          )
        }
        control={
          <div className="setrow__buttons">
            {onRepair && (engine.state === "failed" || engine.state === "needsSetup") && (
              <Button size="sm" variant="primary" icon={<Wrench size={14} strokeWidth={2.2} />} onClick={onRepair}>
                {engine.state === "needsSetup" ? "Set up speech" : "Repair speech setup"}
              </Button>
            )}
            <Button size="sm" variant="secondary" icon={<RotateCw size={14} strokeWidth={2.2} />} disabled={restarting} onClick={() => void restart()}>
              Restart engine
            </Button>
          </div>
        }
      >
        <dl className="specs">
          <div className="spec">
            <dt>Model</dt>
            <dd>{ready?.model || settings.engine.model}</dd>
          </div>
          <div className="spec">
            <dt>Runs on</dt>
            <dd>{runsOn}</dd>
          </div>
          <div className="spec">
            <dt>{memoryLabel}</dt>
            <dd className="num">{memoryValue}</dd>
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
        icon: <Check size={15} strokeWidth={2.5} />,
      });
      setConfirming(false);
    } catch {
      toast.show("Couldn't delete history.", { tone: "danger" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Group id="history" title="History" icon={<History size={15} strokeWidth={2} />} tone="amber">
      <SetRow
        title="Delete all history now"
        desc={`Wren deletes dictations and their recordings after ${retentionLabel(constants.retentionHours)} anyway. This does it now. Your stats and dictionary stay.`}
        control={
          !confirming && (
            <Button size="sm" variant="secondary" className="btn--danger-outline" icon={<Trash2 size={14} strokeWidth={2.1} />} onClick={() => setConfirming(true)}>
              Delete all…
            </Button>
          )
        }
      >
        {confirming && (
          <div className="confirm" role="alertdialog" aria-label="Delete all history?">
            <span className="confirm__text">
              <TriangleAlert size={16} strokeWidth={2.1} /> Delete every dictation and recording? This can't be undone.
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

// ---------------------------------------------------------------- updates

function UpdatesSection({
  version,
  update,
  onCheck,
  onInstall,
}: {
  version: string;
  update: UpdateStatus;
  onCheck: () => Promise<void>;
  onInstall: () => Promise<void>;
}) {
  const titleAndDot = (): { title: string; dot?: string } => {
    switch (update.state) {
      case "available":
        return { title: `Version ${update.version} is available`, dot: "accent" };
      case "downloading":
        return { title: `Downloading v${update.version}…`, dot: "busy" };
      case "installing":
        return { title: `Installing v${update.version}…`, dot: "busy" };
      case "checking":
        return { title: "Checking for updates…", dot: "busy" };
      case "upToDate":
        return { title: "Up to date", dot: "ok" };
      case "error":
        return { title: update.version ? "Update didn't finish" : "Couldn't check for updates", dot: "warn" };
      default:
        return { title: version ? `Wren v${version}` : "Wren updates" };
    }
  };

  const { title, dot } = titleAndDot();
  const pct = update.state === "downloading" ? (update.percent ?? null) : null;

  const control = (() => {
    if (update.state === "available") {
      return (
        <Button size="sm" variant="primary" icon={<Download size={14} strokeWidth={2.2} />} onClick={() => void onInstall()}>
          Update to v{update.version}
        </Button>
      );
    }
    if (update.state === "downloading") {
      return (
        <Button size="sm" variant="primary" icon={<span className="spinner" aria-hidden="true" />} disabled>
          {pct != null ? `${pct}%` : "Downloading…"}
        </Button>
      );
    }
    if (update.state === "installing") {
      return (
        <Button size="sm" variant="primary" icon={<span className="spinner" aria-hidden="true" />} disabled>
          Restarting…
        </Button>
      );
    }
    if (update.state === "checking") {
      return (
        <Button size="sm" variant="secondary" icon={<span className="spinner spinner--dark" aria-hidden="true" />} disabled>
          Checking…
        </Button>
      );
    }
    if (update.state === "error" && update.version) {
      return (
        <div className="setrow__foot-actions">
          <Button size="sm" variant="ghost" icon={<RotateCw size={14} strokeWidth={2.2} />} onClick={() => void onCheck()}>
            Check again
          </Button>
          <Button size="sm" variant="primary" icon={<Download size={14} strokeWidth={2.2} />} onClick={() => void onInstall()}>
            Retry update
          </Button>
        </div>
      );
    }
    return (
      <Button size="sm" variant="secondary" icon={<RotateCw size={14} strokeWidth={2.2} />} onClick={() => void onCheck()}>
        Check for updates
      </Button>
    );
  })();

  return (
    <Group id="updates" title="Updates" icon={<Download size={15} strokeWidth={2} />} tone="violet">
      <SetRow
        title={title}
        dot={dot}
        desc={`You're running ${version ? `v${version}` : "this build"}. Updates come from GitHub and keep your history, dictionary and settings.`}
        control={control}
      >
        {update.state === "available" && update.notes && (
          <div className="update-notes" aria-label="What changed in this update">
            <div className="update-notes__label">What's new</div>
            <pre className="update-notes__body">{update.notes}</pre>
          </div>
        )}
        {update.state === "downloading" && (
          <div className="update-progress">
            <div className="update-progress__track" role="progressbar" aria-label="Download progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct ?? undefined}>
              <div className={`update-progress__fill ${pct == null ? "is-indeterminate" : ""}`} style={pct != null ? { width: `${pct}%` } : undefined} />
            </div>
            <div className="update-progress__meta num">
              {update.totalBytes ? `${formatBytes(update.downloadedBytes)} of ${formatBytes(update.totalBytes)}` : formatBytes(update.downloadedBytes)}
              {" · Wren will restart once the download is verified."}
            </div>
          </div>
        )}
        {update.state === "installing" && (
          <p className="setrow__note">Wren is closing to install the update and will open again in a moment.</p>
        )}
        {update.state === "upToDate" && (
          <div className="setrow__foot">
            <Status tone="ok">You're on the latest version.</Status>
            <span className="setrow__aside">Checked {whenLabel(update.checkedAt).toLowerCase()}</span>
          </div>
        )}
        {update.state === "error" && (
          <div className="setrow__foot">
            <Status tone="warn">{update.message}</Status>
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
  focus,
  update: appUpdate,
  onCheckUpdate,
  onInstallUpdate,
  themeChoice,
  onThemeChange,
  onRepairSpeech,
}: {
  engine: EngineStatus;
  others: string[];
  onSaved: (s: SettingsData) => void;
  focus?: { group: "engine" | "updates" | "meetings"; at: number } | null;
  update: UpdateStatus;
  onCheckUpdate: () => Promise<void>;
  onInstallUpdate: () => Promise<void>;
  themeChoice: ThemeChoice;
  onThemeChange: (next: ThemeChoice) => Promise<void>;
  /** Re-check the speech runtime and models and fix what is broken (opens the setup screen). */
  onRepairSpeech?: () => void;
}) {
  const constants = useConstants();
  const isMac = constants.platform === "macos";
  const [settings, setSettings] = useState<SettingsData | null>(null);
  const [version, setVersion] = useState("");
  const toast = useToast();
  const latest = useRef<SettingsData | null>(null);

  const [failed, setFailed] = useState(false);
  const load = useCallback(() => {
    setFailed(false);
    void getSettings()
      .then((s) => {
        latest.current = s;
        setSettings(s);
      })
      .catch(() => setFailed(true));
  }, []);

  useEffect(() => {
    load();
    void appVersion()
      .then(setVersion)
      .catch(() => {});
  }, [load]);

  const scrollToGroup = useCallback((id: string) => {
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const heading = document.getElementById(`set-${id}`);
    heading?.scrollIntoView({ block: "start", behavior: reduced ? "auto" : "smooth" });
    heading?.focus({ preventScroll: true });
  }, []);

  const loaded = settings != null;
  useEffect(() => {
    if (!loaded || !focus) return;
    const id = requestAnimationFrame(() => scrollToGroup(focus.group));
    return () => cancelAnimationFrame(id);
  }, [loaded, focus, scrollToGroup]);

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
          <p className="page__lede">Changes save as you make them.</p>
        </div>
      </header>

      {!settings && failed && <LoadError what="your settings" onRetry={load} />}
      {settings && (
        <>
          <nav className="settings-index" aria-label="Settings sections">
            {SECTIONS.map((s) => (
              <button key={s.id} type="button" className="bare" onClick={() => scrollToGroup(s.id)}>
                {s.label}
              </button>
            ))}
          </nav>

          <div className="setgroups">
            <AppearanceSection
              choice={themeChoice}
              isMac={isMac}
              onSelect={(next) => {
                if (latest.current) {
                  latest.current = { ...latest.current, theme: toSettingsTheme(next) };
                  setSettings(latest.current);
                }
                void onThemeChange(next);
              }}
            />
            <MicSection settings={settings} update={update} isMac={isMac} />

            <Group id="dictation" title="Dictation" icon={<Keyboard size={15} strokeWidth={2} />} tone="violet">
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
                title={isMac ? "Start at login" : "Start with Windows"}
                desc={
                  isMac
                    ? "Open Wren quietly in the menu bar when you sign in to your Mac."
                    : "Turn this on once Willow and Glaido no longer start with Windows, so only one app listens for the keys."
                }
                control={
                  <Toggle
                    checked={settings.startWithWindows}
                    onChange={(v) => void update({ startWithWindows: v })}
                    label={isMac ? "Start at login" : "Start with Windows"}
                  />
                }
              />
            </Group>

            <WatcherSection settings={settings} update={update} />
            <AiSection settings={settings} update={update} isMac={isMac} />
            <MeetingNotesSection settings={settings} update={update} isMac={isMac} />
            <EngineSection engine={engine} settings={settings} isMac={isMac} onRepair={onRepairSpeech} />
            <HistorySection />

            <Group
              id="others"
              title="Other dictation apps"
              icon={others.length ? <TriangleAlert size={15} strokeWidth={2} /> : <ShieldCheck size={15} strokeWidth={2} />}
              tone="amber"
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

            <UpdatesSection version={version} update={appUpdate} onCheck={onCheckUpdate} onInstall={onInstallUpdate} />

            <footer className="about">
              <WrenTile size={38} />
              <div>
                <div className="about__name">
                  Wren <span className="about__version num">{version ? `v${version}` : ""}</span>
                </div>
                <div className="about__line">Free and local. Your voice never leaves {isMac ? "this Mac" : "this PC"}.</div>
              </div>
            </footer>
          </div>
        </>
      )}
    </div>
  );
}

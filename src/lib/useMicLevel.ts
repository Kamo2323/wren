// Live microphone level for the Settings test. Inside Tauri it opens the chosen microphone through the
// WebView (matched by its Windows name, which is what cpal reports too) and reads RMS from an analyser;
// in a plain browser (mock mode, screenshots) it plays a believable speech-like level instead.
import { useEffect, useState } from "react";
import { inTauri } from "./api";

/** `device` is the label of the track actually being read; `fellBack` is true when that isn't the
 * microphone that was asked for (so a silent switch to the default is visible). */
export type MicTest =
  | { state: "off" }
  | { state: "on"; level: number; device: string | null; fellBack: boolean }
  | { state: "error"; message: string };

/** RMS (0..1) → perceptual 0..1: -60 dBFS is silence, 0 dBFS is full. */
export function perceptual(rms: number): number {
  if (rms <= 0) return 0;
  const db = 20 * Math.log10(rms);
  return Math.min(1, Math.max(0, (db + 60) / 60));
}

const MOCK_DEFAULT_MIC = "Microphone (Shure MV7)";

export function useMicLevel(active: boolean, deviceName: string | null): MicTest {
  const [test, setTest] = useState<MicTest>({ state: "off" });

  useEffect(() => {
    if (!active) {
      setTest({ state: "off" });
      return;
    }
    let raf = 0;
    let stopped = false;
    let stream: MediaStream | null = null;
    let ctx: AudioContext | null = null;

    if (!inTauri) {
      // mock: syllable-like bursts
      const t0 = performance.now();
      const loop = (t: number) => {
        const s = (t - t0) / 1000;
        const env = Math.max(0, Math.sin(s * 5.1) * 0.5 + Math.sin(s * 1.7) * 0.35 + 0.3);
        setTest({ state: "on", level: Math.min(1, 0.18 + env * 0.62), device: deviceName ?? MOCK_DEFAULT_MIC, fellBack: false });
        if (!stopped) raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
      return () => {
        stopped = true;
        cancelAnimationFrame(raf);
      };
    }

    (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false } });
        if (deviceName) {
          const devices = await navigator.mediaDevices.enumerateDevices();
          const match = devices.find((d) => d.kind === "audioinput" && d.label === deviceName);
          const current = stream.getAudioTracks()[0]?.getSettings().deviceId;
          if (match && match.deviceId !== current) {
            stream.getTracks().forEach((t) => t.stop());
            stream = await navigator.mediaDevices.getUserMedia({ audio: { deviceId: { exact: match.deviceId }, echoCancellation: false, noiseSuppression: false, autoGainControl: false } });
          }
        }
        if (stopped) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }
        ctx = new AudioContext();
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 1024;
        ctx.createMediaStreamSource(stream).connect(analyser);
        const buf = new Float32Array(analyser.fftSize);
        const label = stream.getAudioTracks()[0]?.label ?? null;
        let smooth = 0;
        const loop = () => {
          analyser.getFloatTimeDomainData(buf);
          let sum = 0;
          for (const v of buf) sum += v * v;
          const level = perceptual(Math.sqrt(sum / buf.length));
          smooth = level > smooth ? level : smooth * 0.9 + level * 0.1; // fast attack, slow release
          setTest({ state: "on", level: smooth, device: label, fellBack: deviceName != null && label !== deviceName });
          if (!stopped) raf = requestAnimationFrame(loop);
        };
        loop();
      } catch {
        const osLabel = typeof navigator !== "undefined" && /mac/i.test(navigator.platform || navigator.userAgent) ? "macOS" : "Windows";
        if (!stopped) setTest({ state: "error", message: `Couldn't open the microphone. Check it's plugged in and allowed in ${osLabel} privacy settings.` });
      }
    })();

    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close();
    };
  }, [active, deviceName]);

  return test;
}

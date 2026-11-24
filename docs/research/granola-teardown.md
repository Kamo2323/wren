# Granola desktop teardown (Windows build 7.821.0)

Static analysis only; no login. Target: `C:\Users\maxha\AppData\Local\Programs\@granoladesktop`.
App: Electron, Vite/rolldown bundles; `resources/app.asar` (sha256 `6807198d…74ba`) extracted to scratch.
REA evidence: JS graph `ev_a74ab379…c6ce` (summary `ev_fdc7b241…`), fuses `ev_044ab05f…`,
native exports `ev_f77e90d0…`, PE imports `ev_08c3ce12…`. Everything below is from shipped code
(observed) unless marked *inferred*. Feature-flag defaults are local defaults; the server
overrides them, so "default false" does not mean "off in production".

Key files (inside asar):
- `dist-electron/main/index.js` (3 MB, main process), `dist-app/assets/primary-8zmoxAYO.js` (7.4 MB renderer)
- `dist-electron/{local_asr,mic_monitor_v2,macos_mic_apps_with_devices,third_party_meeting_automation,screen_capture}_process/index.js` (Electron utilityProcess workers)
- Native: `resources/native/windows/granola.native.windows.win32-x64-msvc.node` (Rust napi-rs),
  `resources/native/windows-third-party-meeting-automation/GranolaThirdPartyMeetingAutomation.exe` (C++ UIAutomation),
  macOS-only `.node` names referenced: `macos_mic_apps_with_devices`, `ax_meeting_detector`, `screen_capture`, `nub`, `microphone_mode`, `tcc`, `third_party_meeting_automation`.

## 1. Audio capture

**Architecture.** Capture runs in a hidden renderer ("audio process"), not native code. Main sends
`to-audio-process {event:'start-audio-capture', sampleRate: op}` where `op=16e3` (main `pF.startAudioCapture` / `UYe`).
Renderer returns `audio-capture-buffer {microphoneBuffer, systemAudioBuffer, capturedTimestamps}`.
- Two independent **mono 16 kHz s16** streams, `microphone` and `system`; never mixed. AudioWorklet
  `audio-capture-processor` posts 256-sample Float32 batches (`audioWorkletProcessor-2QEPQrXR.js`).
  Main batches per source into 100 ms chunks (class `tXe`, `bytesPerBatch = sr*2*100/1000`).
- **Mic**: `getUserMedia({audio:{echoCancellation:{ideal:'all'}, autoGainControl:false, deviceId, sampleRate}})`
  (renderer `RMr`). `'all'` = Chromium system-wide AEC (uses all system playback as echo reference, not
  just this page). Server flag `transcription_mic_capture_processing` can disable EC / NS / all processing.
- **System audio, Windows**: `getDisplayMedia({audio:{sampleRate},video:false})`, main
  `session.setDisplayMediaRequestHandler` answers `{audio:'loopback'}` (Chromium WASAPI loopback of the
  default render device) or `{audio:{id:'loopbackAllDevices'}}` (`kVe`: only when flag on, process loopback
  supported, no "loopback-blind" app, mic detection available).
- **Teams on a non-default output device (Windows)**: native `startDeviceRenderLoopback(deviceId, sr, onPcm, onState)`
  in `mic_monitor_v2_process`; picks the render endpoint where Teams' audio session has peak > 1e-6
  (`probeRenderSessions(pids)` over `ms-teams.exe`, `ms-teams_modulehost.exe`, `msteamsvdi.exe`, `msedgewebview2.exe`),
  re-targets every 3 s, reopens if silent. PCM is piped to the renderer as a fake track via
  `MediaStreamTrackGenerator` + `AudioData` (`DMr`/`OMr`, event `native-system-audio`). Flag `windows_teams_device_loopback`.
- **System audio, macOS**: same `getDisplayMedia`; handler uses `desktopCapturer.getSources({types:['screen']})`
  + `audio:'loopback'` (Electron's ScreenCaptureKit path) on macOS < 14; on macOS ≥ 14 (`fE`) it returns the
  `loopbackAllDevices` audio-only source. Unified-log predicate watches `coreaudiod`, `replayd`,
  `ScreenCaptureKit`. No Core Audio process-tap code found in JS (*may live in the macOS-only native module, not shipped here*).
- **Echo / bleed**: AEC on mic only; no separate JS echo-dedup between streams found. Transcript
  rows can carry `speakerSuppressed` (renderer) but its trigger was not traced.
- **Storage**: audio is **not stored** by default. Debug flag `save_audio` (default false) writes
  `userData/audio_files/<ts>_microphone.wav` + `_system.wav` (`oGe`). DB column `audio_file_handle` exists (*server-side recording possible*).
- Recovery: silence watchdog restarts system capture (`audio-capture-system-silence-restart`), up to N restarts.

## 2. Meeting detection

- **Mic-in-use, Windows**: native `MicActivityTrackerV2` (WASAPI capture-endpoint audio sessions; events
  `session_active` / `session_inactive` with pid + processName) in `mic_monitor_v2_process`. Process names map
  to app ids via table `tO` (zoom.exe, ms-teams.exe, teams.exe, chrome.exe, msedge.exe, slack.exe, webex.exe,
  discord.exe, whatsapp.exe, ~35 entries). Browsers count, so Meet in Chrome triggers "Meeting detected".
- **Mic-in-use, macOS**: `macos_mic_apps_with_devices.node` `getActiveInputProcesses()` polled every 1 s
  (bundleId, pid, deviceUID); helper bundles folded to parent (`WBe`, e.g. Chrome helper → Chrome).
- **Extra signals**: Google/Outlook calendar (`GOOGLE_CALENDAR_POLLING`), scheduled reminders
  (`meeting_notification_scheduling_window_ms` 10 min), Chrome extension for Meet (`native-host/chrome-extension-host.exe`),
  macOS `ax_meeting_detector` (flag), Windows UIAutomation helper finds meeting windows (`MeetingFinder`).
- **Prompt** (`mft` in main): delay `mic_apps_notification_delay_seconds`=2 s, then a custom always-on-top
  404-px panel window "Notification": title **"Meeting detected"** (FaceTime/WhatsApp → "Call detected",
  Slack → "Huddle detected"), body **"Take notes with Granola"**; if a calendar event is live, title = event
  summary, body = "10:00 – 10:30". Buttons "Take notes", menu "Turn off notifications for <app>",
  "Change Notification Settings". Per-app silencing (`silencedAppIdsForImpromptuNotifications`).
  Optional auto-start countdown ("Auto-starting…", `impromptu_notification_auto_start_seconds`=20).
  Calendar variant offers "Join Meeting" / "Join without Granola".
- **Auto-stop** (renderer `u7n`): when no mic app remains, and transcript ≥ 3 min: stop if within ±5 min of
  calendar end; otherwise ask server LLM prompt `did-meeting-end` with the transcript tail. UI shows
  "Stopping in 60 seconds" countdown and "Granola will stop transcribing".

## 3. Transcription

- **Cloud, streaming WebSocket per source** (one socket for mic, one for system; `channels:1`).
  - Default `transcription_provider` = **Deepgram** `wss://api.deepgram.com/v1/listen`, model **nova-3**
    (`nova-3-mega` for "58-lang-beta"), options `{smart_format, interim_results, encoding: linear16|opus,
    sample_rate, keyterms, mip_opt_out}`; multi-language adds `language:'multi', endpointing:'100'`.
    Auth: short-lived token fetched from Granola backend, passed as WS subprotocol `['bearer', token]`.
  - Alternatives: **AssemblyAI** Universal Streaming v3 (`wss://streaming[.us|.eu].assemblyai.com/v3/ws`,
    `universal-streaming-english/-multilingual`, turn params `end_of_turn_confidence_threshold .8,
    min_end_of_turn_silence_when_confident 800, max_turn_silence 2400`, optional `speaker_labels`),
    `grok-transcribe-2.0`, `elevenlabs-scribe-v3-live`. Optional Cloudflare relay (`cloudflare_transcription_relay_enabled`).
  - Fallback chain (`transcription_provider_fallback_budget`=4), liveness timeouts (Deepgram 30 s, AAI 10 s), stall redial.
  - Keyterms = user dictionary + attendee/company names (*inferred from `keyterms`/`user_dictionary`*).
  - Opus streaming optional (`opusscript` dep, flag `transcription_opus_streaming`).
- **Local ASR (fallback / opt-in)**: `local_asr_process` runs **sherpa-onnx Parakeet TDT 110M int8** (English,
  hotwords) or **whisper.cpp `whisper-server` v1.9.1** + `ggml-small.en-q5_1` (Metal on darwin-arm64, CPU on win32),
  models from a CloudFront CDN with sha256 pinning. Segmenter: 16 kHz, 512-sample hops (32 ms), energy VAD
  (RMS ≥ 0.01), segment closes after 25 silent hops (~800 ms) or 25 s max, partials every 31 hops (~1 s),
  8-hop pre-roll. Used as a **"local bridge"** when the cloud socket drops (`transcription_local_bridge`).
- **Languages**: UI offers "English (best quality)" vs "Auto"; optional language hints.
- **Speaker labels** (renderer label functions): `source==='microphone'` → **"Me"**, `system` → **"Them"**;
  upgraded to a real name when `detectedSpeaker.participantName` exists; AssemblyAI diarization yields "Speaker A/B";
  room systems shown as "[Room] Name". Bubbles: Me right-aligned, Them left (`NubTranscriptBubble`).
- **Participant names: yes, several ways.**
  1. Calendar attendees (`google_calendar_event.attendees`, manual edits) → prompt vars `participants`, `their_name`, companies.
  2. **Live active-speaker reading from meeting apps**: `third_party_meeting_automation_process` →
     `monitorZoomSpeakers`, `monitorTeamsSpeakers`, `monitorMeetChromeSpeakers` (UIAutomation on Windows via
     `GranolaThirdPartyMeetingAutomation.exe` / CUIAutomation8; AX on macOS), plus screenshot/OCR variants
     (`*_screen_capture_speaker_attribution` off|assist|only, Zoom local OCR or cloud reader, Haiku fallback).
     Lag compensation `speaker_detection_lag_ms` (Teams Win 900 ms, Zoom Win 1400 ms). Also `readZoomSelfName`,
     `readTeamsParticipantCount`, `readTeamsMeetingId`.
  3. LLM `speaker-name-suggestions` / `summary-name-corrections` prompts; table `speaker_name_suggestions`.
- Consent: can auto-paste a "Granola is transcribing" message into Zoom/Teams/Slack chat (`runElectronChatPaste`).

## 4. Notes UX and AI

- Editor: TipTap/ProseMirror + Yjs (`collaboration`, table `ydocs`). Note views are tabs **`private`**
  ("My notes"), **`enhanced`** ("Enhanced notes"), **`transcript`**. Placeholder: "Write notes, or press '/' for templates".
- **Enhanced notes = a "panel"** (`document_panels`: `template_slug`, `content`, `original_content`,
  `generated_lines`, `suggested_questions`). Generated server-side and streamed (`panel-stream-event delta|done|error`).
  "Generate notes" / "Regenerate notes" / "Enhancing notes".
- **User vs AI text**: extension `aiWrittenText` adds a decoration class to textblocks whose block ids are
  in the AI-written set; `authorAttribute` stores `data-author` on blocks; `generatedLines` tracks initial generated
  lines; `view-source-*` classes let a line link back to transcript evidence (prompt `view-source-summary`).
  (*Classic Granola look: user text black, AI text grey — the colour itself is in CSS, not traced.*)
- **Prompts are not in the bundle.** Client sends `{prompt_slug, prompt_variables, document_id}` to the backend.
  Summary slugs (`vKe`): `meeting-summary-consolidated` (default), `notes-centric-{very-short,short,long}-input`,
  `template-summary-consolidated`, `collaborative-meeting-summary`, … Chat slugs: `chat`, `live-chat`,
  `what-did-i-just-miss`, `what-does-that-mean`, `make-me-sound-smart`, `list-action-items`,
  `suggest-follow-up-questions`, `nextgen-chat-live-meeting`. Utility: `meeting-title`, `did-meeting-end`,
  `will-meeting-end`, `slack-summary`, `off-record-detector`, `chat-thread-title`.
- **Prompt variables** (`gatherPromptInputsForDocument`): `transcript`, `notes` (user's notes markdown),
  `participants`, `calendar_event_title`, `my_name/my_bio/my_company/my_colleagues`, `their_name/their_bio`,
  `external_attendees/companies`, `user_dictionary`, `todays_date`, `is_multi_language`, `has_long_user_notes`,
  `has_speaker_attribution`, `transcript_numbered`, `latest_meeting_summary`.
- **Templates**: server `panelTemplates` with `sections[{heading, section_description}]`; client builds
  `summary_headings` = `#Heading: description` lines, `first_heading`, `meeting_description`, `template_title`,
  `max_num_headings` (4 if transcript < 8k chars, 5 if < 20k, up to 8). Headings get `data-template-heading`.
- **Recipes** = saved chat prompts, inserted as atom nodes via `/` (or `@`) with `slug`.
- **Chat with meeting**: "Ask anything" bar (main window and nub), multi-meeting chat, citations.
- **Sharing/outputs**: share link (public / internal attendees automatically), "Shared with me",
  Slack summary, follow-up email draft, PDF export (`printToPDF`), Notion/HubSpot/Attio/Affinity, MCP access for agents.

## 5. UI layout (sketchable)

- **Primary window**: left sidebar (notes list, folders/"spaces", Shared with me), centre = note.
  Note header: title + calendar chip + attendees. Body: tab switch My notes / Enhanced / Transcript.
  Bottom: floating "Ask anything" chat input; transcription control pill with "Show transcript" /
  "Hide transcript" toggle and a waveform; warning dot when capture is unhealthy.
- **Nub** (floating indicator while transcribing): frameless, transparent, always-on-top, non-focusable
  panel **42 px wide**, docked to the **right screen edge** (draggable to left edge), vertically centred.
  Expands on hover into a panel with live transcript bubbles, "Ask anything", "Open note", pause/stop,
  "Transcribing…"; separate "Granola transcription notice" strip (52 px) listing participants.
- **Meeting-detected toast**: 404-px custom panel near the top-right (optional follow-cursor), app icon +
  title/body + "Take notes" split-button.
- **Post-meeting**: on stop, enhanced panel generation starts (summary first paint); user lands on the Enhanced
  tab with template picker, "Regenerate notes", share/copy, chat.

## 6. Storage

- `%APPDATA%\Granola\granola.db` — **SQLCipher** (`better-sqlite3-multiple-ciphers`, `pragma cipher='sqlcipher'`,
  raw hex key). Tables: `documents` (notes, notes_markdown, google_calendar_event, people, summary, overview,
  selected_template, transcription_metadata, audio_file_handle, …), `document_panels`, `ydocs` (Yjs updates),
  `document_attendees`, `document_tags`, `calendar_events`, `speaker_name_suggestions`, `pre_meeting_briefs`,
  `dictation_*`, `sync_outbox`, `sync_state`. Local-first with server sync via outbox.
- `cache-v6.json.enc` (app state incl. `transcripts:{}`, `panelTemplates`), `user-preferences.json.enc`,
  `stored-accounts.json.enc`: encrypted with a random 32-byte DEK stored in `storage.dek`, which is wrapped by
  Electron `safeStorage` (DPAPI / Keychain).
- No raw audio kept (see §1). Transcripts and notes stay local and are also synced to Granola's cloud.

## Not determined

- Exact enhancement / summary prompt text (server-side by slug); server-chosen flag values in production.
- Visual styling of AI vs user text and the exact main-window layout (no logged-in screenshots).
- Whether macOS uses a Core Audio process tap (macOS native modules not shipped in this Windows build).
- How `speakerSuppressed` (possible echo/bleed dedup) is triggered.

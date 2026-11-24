# Notion AI Meeting Notes ("Notetaker") — teardown
Target: Notion desktop 7.44.1 (MSIX, Electron) on Windows 11, examined 2026-11-24. Read-only static analysis:
no recording was started and no workspace content was opened or changed.
### Sources
- `resources/app.asar` (extracted): main process `.webpack/main/index.js` plus local renderers `meeting_notification_pill` and `speaker_and_microphone_status`.
- Native: `app.asar.unpacked/node_modules/@notionhq/desktop-native/{module.js, build/Release/win_utils.node, linux/*.js}` and `resources/notion-meeting-notes-host.exe`.
- Web client: almost all meeting-notes UI and logic is **not in the asar**. It is the notion.so web app, cached at `%APPDATA%\Notion\Partitions\notion\Service Worker\CacheStorage` (`app.notion.com/_assets/*.js`). Chunks below are cited by asset name.
- User data: `%APPDATA%\Notion\NativeMessagingHosts\*.json` and `logs\*.log`. This is a fresh install with no meeting history.
- REA: `inspect_pe_imports` on the host exe (evidence `ev_4ff5e976…`). `analyze_javascript_application` rejected the asar (integrity mismatch on unpacked `.node` files), so the bundles were extracted with `@electron/asar` and grepped.
Everything below is an **observed** string or code path unless it is marked *(inference)*.
Server behaviour (prompts, which vendor is used) cannot be seen from the client.

---

## 1. Audio capture
**Architecture.** Capture is done in the web renderer with standard web APIs. Electron only grants and steers it.
There is no custom native capture module.
### Microphone
- Uses `getUserMedia({echoCancellation:!1, noiseSuppression:!1, autoGainControl:!1, channelCount:1})`.
- An optional flag adds `echoCancellation:"all"`, Chromium's system-wide AEC (`75247-*.js`, `audioRecorderHelpers`).
- There is a device picker ("Mic" in block settings), a 15 s acquisition timeout (`meeting_notes_media_acquisition_config`), and retries.
### System audio (both OSes)
- The renderer calls `getDisplayMedia({video:<isLinux>, audio:true, systemAudio:"include"})`.
- The main process answers through `setDisplayMediaRequestHandler(…, {useSystemPicker:false})`, for Notion origins only.
### Windows system audio
- The handler returns `{video: desktopCapturer screen[0], audio:"loopback"}`. That is Chromium's built-in WASAPI loopback of the whole render device.
- `win_utils.node` does contain WASAPI code (`IAudioClient`, `Initialize(shared, LOOPBACK)`, `IAudioCaptureClient`), but only for the **diagnostics** window (`fullAudioDiagnostics`, "Speaker and Microphone Status…").
- The OS gate is Windows 10 build 19041 or later (`getDesktopOSSupportForKernel`). *(Inference: that matches the process-loopback API's minimum build.)*
### macOS system audio (main `coreAudioTapController`)
- On macOS 14.2+ Notion enables the Chromium feature `MacCatapLoopbackAudioForScreenShare`: a Core Audio **Process Tap**, returned as `{audio:"loopback"}`.
- That path is gated by a server flag (`coreAudioTapGateEnabled`) and a user opt-out (`coreAudioTapOptOut`).
- Otherwise it falls back to **ScreenCaptureKit** (screen source plus loopback audio).
- Permissions come from `mac_utils.node`, which is not in the Windows build. Its exports, per `module.js`:
  - microphone: `requestMicrophoneAccess`
  - screen recording: `requestScreenCaptureAccess`
  - audio capture, the TCC permission used by taps: `requestAudioCaptureAccess`
  - Accessibility, for Zoom: `checkAccessibilityAccess`
- Minimum is macOS 13 ("Transcribing on AI Meeting Notes requires macOS 13 or later").
### Channel separation: yes, per source
- `AudioRecorder` creates one `AudioWorkletNode("pcm-processor")` per stream. Stream 0 is tagged `"microphone"`, stream 1 `"system_audio"` (`splitBySource`).
- A third worklet gets the mixed signal for the archive and failover copy (`onArchiveDataReceived`).
- Audio is downsampled to 16 kHz Float32 (`sampleRate ?? 16e3`). Frames are 192 ms when split, 96 ms in mono (`meeting_notes_audio_frame_window_config`, valid range 96–960).
- Each frame goes out as `audioData {base64Audio, audioSource:"microphone"|"system_audio", language, blockId, spaceId, userId}`. `base64Audio` is raw Float32 bytes, made with `FileReader.readAsDataURL(Blob)` (module `392719`, fn `l`).
### Permission and consent UX
- A one-time "AI Meeting Notes Setup" modal (`17948-*.js`, `transcriptionSetupSteps.*`) has four steps: "Update the Notion app" → "Enable microphone access — This allows Notion AI to transcribe what you say." → "Enable system audio access — …transcribe what everyone else is saying (even on headphones)." → "Restart Notion"
- The permission test opens `getDisplayMedia` once and stops it immediately.
- Windows troubleshooting points to Privacy & security → Microphone. Loopback is treated as covered by that toggle (`finding.loopbackDenied`).
- Social consent:
  - start footer: "By starting, you confirm everyone being transcribed has given consent."
  - an optional audio consent message, which admins can enforce
  - an automatic chat disclosure in Meet (through the extension) and Zoom (`sendZoomMeetingConsentMessage`, macOS)
  - a Meet camera badge, and email-domain allow/block/warn rules
- Extras:
  - VDI: `RemoteMeetingNotesProxy` over an RDP DVC or a Citrix channel (`WTSVirtualChannelOpenEx`)
  - Linux: `audio_proxy` (JSON lines; `audio:frames` carries Float32)

## 2. Meeting detection and start UX
### Calendar (Notion Calendar / Google)
- The tray/menu-bar schedule shows "Upcoming", "Connect calendar", and an optional preview with a countdown.
- The upcoming-meeting notification is a frameless, always-on-top window centred at the top of the screen. It loads web route `/meeting-notification`.
  - Copy: "Now {timeRange}"
  - Buttons: **Join and transcribe** / **Join meeting** / **Start transcribing**
  - Consent footer, plus "Manage notification settings"
### Call detection (setting "Detects when calls start, and auto-stops transcription when you leave calls")
- The main process polls three signals:
  - which processes hold the mic or speakers (`getProcessesAccessingMicrophone*`; Windows uses `IAudioSessionManager2`)
  - UDP sockets (`GetExtendedUdpTable`; Meet 19302–19309, Zoom 8801+)
  - audio level above -50 dBFS
- Known clients: Zoom, Teams, Slack, Webex, plus Chrome, Edge, Brave, Arc, Firefox, Safari, Atlas, Comet (`audioProcessIds`).
- The snapshot is sent to the server in `clientInfo.meetingCallDetection`.
- When the call app releases audio before the calendar end, the pill asks "Did your meeting end? Ready to summarize?" with **I'm done** / **Not yet**.
- Calendar auto-stop is armed 5 minutes before the end.
### The block
- A `transcription` block, or a `meeting_note_page` in a "Meetings" database.
- Created from the slash/quick-search command, a calendar event, or the Meetings home (`/home/meetings`).
- Start button: "Start transcribing ▾". The dropdown offers "Start in browser" (mic only) or "Open in Notion app".

## 3. Real-time transcription
### Transport
- Notion's own **AudioProcessor** service over a **Primus WebSocket** (`17048-*.js`).
- RPC methods: `clientInfo`, `transcriptionInfo`, `transcriptionSettings`, `audioData` (each acked), `audioFinished`, `drainTranscription`, `destroySession`.
- Server pushes:
  - `transcription`, `transcription_speaker_revision`, `audio_writer_settings`
  - `service_status_update`, `free_trial_limit_update`
  - errors: `rate_limited`, `openai_error`, `live_transcription_unrecoverable`
- A circuit breaker moves the session into **failover**: local recording continues and the transcript is rebuilt after stop.
### Vendors (the server chooses)
- `audio_writer_settings.transcription_model` ∈ `openai_diarization | assemblyai_diarization | baseten_diarization | fireworks_diarization | xai_diarization | no_diarization`.
- Route IDs include `openai_streaming`, `assemblyai_streaming`, `baseten_v1–v3`, `xai_streaming`.
- Statsig defaults (`DNUtl-*.js`):
  - interim `openai-gpt-4o-mini-transcribe`, final `openai-whisper-1`, auto-language final `openai-gpt-4o-transcribe`
  - a Fireworks diarization endpoint
  - Baseten with VAD and 1 s partials
- A dev-only "Override Model" menu lists all of them.
### Segment schema
- `{segmentId, isSegmentComplete, transcription:{status: pending|complete|error|ignore, text, audioStartMs, audioEndMs}, audioSource, speakerName, segmentSortkey}`.
- Live text arrives as pending/interim segments and is then finalised.
- `transcription_speaker_revision` relabels segments that were already rendered.
### Speaker names: yes, when they are known
1. **Google Meet**, through the "Notion Meetings Add-on" Chrome extension.
   - Path: native-messaging host `com.notion.meeting_notes(.v2)` → `notion-meeting-notes-host.exe` → pipe `\\.\pipe\LOCAL\notion-meeting-notes-*` (bridge token, plus `WinVerifyTrust` of the peer) → main process.
   - It sends `speaker_activity`, mic mute state, `participant_roster`, `local_identity`, and consent chat.
2. **Zoom on macOS**, through an Accessibility observer (`startZoomMeetingObserver`). There is no Windows equivalent.
3. **Source plus calendar.** Label intents:
   - `recorder_person`: mic → the recording user
   - `system_audio_attendee_person`: system audio → a calendar attendee *(inference: 1:1 calls)*
   - `system_audio_generic_other_person` → "Others"
- Fallbacks: "{name}'s microphone", "{name}'s system audio", diarization "Speaker {n}", "Unknown".
- Attribution has a confidence score based on overlap: "Attribution: {speakerName} ({confidence})".
### Language and vocabulary
- Per-block language: "Auto detect" plus about 23 languages.
- "Transcript hints" is a vocabulary list at user and workspace level.

## 4. Summary and enhanced notes
### Tabs
- Module `839464`: `["summary","notes","transcript"]`, shown as **Summary / Notes / Transcript**. Each tab is a child block (`transcription_summary_id`, etc.).
- The default tab is Summary when it has content, otherwise Notes.
- An experiment renames Notes to "Agenda".
- Notes are free-form user notes. Placeholder: "Notion AI will summarize the notes and transcript".
### Generation
- "Stop and summarize" starts the server AI step `summarize-transcript`.
- **The prompt is not in the client.** A bounded grep found none.
- Progress copy, in order: "Reading transcript…" → "Organizing discussion topics" → "Pulling out key decisions" → "Finding action items" → "Thinking about how to refer to people" → "Searching your workspace…" / "Searching for people" (it @-mentions people and pages) → "Checking details against the transcript" → "Finalizing summary"
- The summary carries **citations** into the transcript.
- Regenerate asks "This will re-generate your summary. Continue?".
### Formats and follow-up
- "Summary instructions": **Auto** (the default) plus custom instruction templates that can be set as default.
- Section headings are not hard-coded in the client. *(Inference: chosen server-side — overview, decisions, action items with assignees.)*
- Afterwards: auto-share with attendees, Slack/email templates, a link on the Google Calendar event, thumbs feedback, and an internal A/B "Which meeting summary is better?".

## 5. UI layout (for sketching)
### Block
- Title row with a calendar-event chip and attendee avatars (Share).
- Tabs: Summary | Notes | Transcript.
- On the right: the record control and a "⋯" menu.
### Record control
- States: "Start transcribing ▾" → "Starting" → **Pause / Resume / Stop** (Stop tooltip "Stop and summarize") → "Transcribing" / "Summarizing" / "Thinking".
- Status line: "**You** are currently transcribing this meeting", or "{name} is …" when another device is transcribing (hand-off).
- Condensed controls: Pause, "Stop transcribing", "Always get consent before transcribing others".
### ⋯ menu
- Mic device, Audio: System, language
- Instructions, Transcript hints, Consent
- Audio recordings ("Stored on your device"), Pause/Resume
- Delete transcript / audio / playback media
- Open session logs, Give feedback
### Transcript tab
- Speaker runs, each with an avatar or source icon, a name, and a clickable timestamp ("Play transcript audio from {t}").
- Expand/Collapse, "Scroll to bottom", and an audio player when playback media exists.
- Empty state: "Your meeting will be transcribed here…".
### Floating pill (local renderer)
- Window: 192×74, or 74×192 when docked to the left/right edge. Transparent, always-on-top (screen-saver level), never takes focus, draggable on Windows. Shows a live **waveform**.
- States:
  - transcribing
  - paused: "Transcription paused · Resume"; after 5 min, "paused for more than 5 minutes"
  - idle: "Still there? Hearing crickets… Check your mic or end the meeting?" (I'm here / Stop)
  - error
  - "Another user has begun transcribing"
  - ended: "Did your meeting end?" (I'm done / Not yet)
- No elapsed-timer string was found. *(Inference: no visible clock; the waveform is the "live" signal.)*
- **Quotas** (block, not pill): "You have {n} minutes left", plus a daily cap ("up to {hours} a day").

## 6. Storage and retention
### Transcript
- Stored as Notion blocks in the page.
- Deleting it also deletes the page's version history.
### Audio
- Server playback media can exist ("Delete audio — transcript and timestamps will stay").
- A local failover/archive copy lives in OPFS `notion-opfs-meeting-notes-failover`:
  - FLAC (`meeting-notes-failover-flac`), 16 kHz mono
  - behind the admin setting "Store audio locally — temporarily store a local, on-device copy…"
  - uploaded for recovery after a live failure (limit 1 GB; "may have expired")
  - the user can download or delete it
### Admin and limits
- "Automatic transcript deletion … after a set time" (Enterprise).
- "Zero data retention with LLM providers" (Enterprise).
- Audio upload: 25 MB maximum; aac, m4a, mp3, wav.

---

## Takeaways for Wren
### Capture
- Keep **two tagged streams** (mic = me, system = others). Notion's labels, "Others" fallback, and attribution all depend on that split.
- Raw mic (no AEC/NS/AGC) is the default. AEC is an opt-in for speaker bleed.
- Windows: whole-device WASAPI loopback was enough for Notion.
- macOS: Process Tap on 14.2+, ScreenCaptureKit as fallback.
### Detection and controls
- Detect without a bot: a known call process holding the mic (`IAudioSessionManager2` / Core Audio), combined with the calendar.
- Show a non-focusable pill. Ask "Did your meeting end?" when the call app releases audio.
### Transcription and UI
- Streaming interim text with a final pass, and revisable speaker labels.
- Real names come from integrations or the calendar, not from diarization alone.
- Tabs: Summary / Notes / Transcript. User notes feed the summary, and the summary cites the transcript.
- Always keep a local compressed recording so the transcript can be rebuilt after a network or ASR failure.

## Could not determine
- The summary prompt and section schema (server-side).
- Which ASR vendor is live today. The server decides; the client defaults name OpenAI.
- Whether server-side audio is retained by default.
- The exact live pixel layout. No meeting was run; the layout is reconstructed from strings and component state.
- The macOS native implementation (`mac_utils.node` is not in the Windows build). Its behaviour is inferred from `module.js` exports and main-process callers.

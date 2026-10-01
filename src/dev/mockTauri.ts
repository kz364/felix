/**
 * Browser preview of the settings window without the Tauri backend, for
 * design work: `bun run dev`, then open http://localhost:1420/?mock.
 * Only loaded in development and only with `?mock`; never in the app.
 *
 * Settings come from `settings.local.json` next to this file (gitignored;
 * a scrubbed copy of real settings) or the minimal fallback below.
 */
import {
  mockConvertFileSrc,
  mockIPC,
  mockWindows,
} from "@tauri-apps/api/mocks";

const fixtures = import.meta.glob("./settings.local.json", { eager: true });
const localSettings = (Object.values(fixtures)[0] as { default?: unknown })
  ?.default as Record<string, unknown> | undefined;

const model = (
  id: string,
  name: string,
  downloaded: boolean,
  extra: Record<string, unknown> = {},
) => ({
  id,
  name,
  description: "Fast and accurate on Apple Silicon.",
  filename: `${id}.gguf`,
  source: {
    HuggingFace: { repo_id: `handy-computer/${id}`, revision: "main" },
  },
  size_mb: 640,
  is_downloaded: downloaded,
  is_downloading: false,
  partial_size: 0,
  is_directory: false,
  engine_type: "TranscribeCpp",
  accuracy_score: 0.86,
  speed_score: 0.9,
  supports_translation: false,
  is_recommended: true,
  supported_languages: ["en", "de", "fr", "es"],
  supports_language_selection: true,
  is_custom: false,
  supports_streaming: false,
  supports_language_detection: true,
  ...extra,
});

const MODELS = [
  model("cohere-transcribe", "Cohere Transcribe", true),
  model("qwen3-asr", "Qwen3-ASR 0.6B", true, { is_recommended: false }),
  model("parakeet-v3", "Parakeet V3", false, { supports_streaming: true }),
  model("whisper-turbo", "Whisper Turbo", false, { is_recommended: false }),
];

const now = Math.floor(Date.now() / 1000);
const HISTORY = [
  {
    id: 3,
    file_name: "handy-3.wav",
    timestamp: now - 120,
    saved: false,
    title: "Today, 09:12",
    transcription_text:
      "um so I think we should push the fix to main tomorrow morning",
    post_processed_text:
      "I think we should push the fix to main tomorrow morning.",
    post_process_prompt: null,
    post_process_requested: false,
    has_audio: true,
    transcription_model: "cohere-transcribe",
    context: {
      app_name: "Claude",
      bundle_id: "com.anthropic.claudefordesktop",
      url_host: null,
      declared_category: null,
      category: "coding",
      cleanup_context:
        "Conversation on screen:\nSam: can you ship the login fix today?\n\nText already in the field before the cursor: (empty)",
    },
  },
  {
    id: 2,
    file_name: "handy-2.wav",
    timestamp: now - 3600,
    saved: true,
    title: "Today, 08:14",
    transcription_text: "restart my cloud instance and check the logs",
    post_processed_text: "Restart my Claude instance and check the logs.",
    post_process_prompt: null,
    post_process_requested: false,
    has_audio: true,
    transcription_model: "cohere-transcribe",
    context: {
      app_name: null,
      bundle_id: null,
      url_host: null,
      declared_category: null,
      category: null,
      cleanup_context: null,
    },
  },
];

const benchmarkRecord = (id: string, extra: Record<string, unknown>) => ({
  record: {
    id,
    at: new Date().toISOString(),
    audio: `${id}.wav`,
    seconds: 4.2,
    kept_seconds: 3.9,
    gain_db: 0,
    auto_gain: true,
    gain_at_start: { speech_level: -24, noise_floor: -62 },
    vad_backend: "silero",
    microphone: "MacBook Pro Microphone",
    language: "en",
    app: "Slack",
    bundle_id: "com.tinyspeck.slackmacgap",
    model: "cohere-transcribe",
    error: null,
    edited: null,
    edit: "unchanged",
    ground_truth: null,
    guess: null,
    ...extra,
  },
  audio_path: `/tmp/${id}.wav`,
});

const BENCHMARK = [
  benchmarkRecord("b3", {
    transcript: "um can you run cube cuddle get pods on the uh staging cluster",
    pasted: "Can you run cube cuddle get pods on the staging cluster?",
    edited: "Can you run kubectl get pods on the staging cluster?",
    edit: "edited",
    guess: {
      text: "Um, can you run kubectl get pods on the uh staging cluster",
      confident: true,
      unsure: [],
      notes: "Changed cube cuddle to kubectl, from your edit.",
      by: "ChatGPT gpt-5.5",
      heard: [],
      at: new Date().toISOString(),
    },
  }),
  benchmarkRecord("b2", {
    app: "Mail",
    transcript: "I'll send the the deck over to Sam tomorrow",
    pasted: "I'll send the deck over to Sam tomorrow.",
    ground_truth: "I'll send the the deck over to Sam tomorrow",
  }),
  benchmarkRecord("b1", {
    app: null,
    transcript: "ask cloud to check the logs",
    pasted: "Ask Claude to check the logs.",
  }),
];

const RESPONSES: Record<string, unknown> = {
  get_app_settings: localSettings ?? {},
  get_default_settings: localSettings ?? {},
  get_available_models: MODELS,
  get_current_model: "cohere-transcribe",
  get_history_entries: { entries: HISTORY, has_more: false },
  learned_rules: {
    words: ["kubectl", "GitHub", "Supabase", "LLM"],
    auto_learned: [],
    corrections: [
      { from: "cube cuddle", to: "kubectl" },
      { from: "super base", to: "Supabase" },
    ],
    soundalikes: [
      {
        heard: "cloud",
        word: "Claude",
        meaning: "Anthropic's assistant",
        compounds: ["Claude Code"],
        ordinary_after: [],
        name_in_apps: ["Claude", "Codex"],
        name_if_on_screen: true,
      },
    ],
    error: null,
  },
  report_mistake: {
    applied: true,
    proposal: {
      id: "mock",
      explanation:
        "It heard “cube cuddle” for kubectl. Added a replacement and kubectl to the vocabulary.",
      needs_code_change: false,
      rules: "",
      diff: [
        { kind: "added", text: 'vocabulary = ["kubectl"]' },
        { kind: "added", text: "[[replace]]" },
        { kind: "added", text: 'from = "cube cuddle"' },
        { kind: "added", text: 'to = "kubectl"' },
      ],
      tests: [],
      error: null,
    },
  },
  mistake_reports: [
    {
      id: "r1",
      at: new Date().toISOString(),
      source: "voice",
      report: "it keeps writing super base instead of Supabase",
      transcribed: "store it in super base",
      pasted: "Store it in super base.",
      transcription_model: "cohere-transcribe",
      has_audio: true,
      explanation:
        "The speech model splits Supabase into two words. A correction now joins them.",
      change: ['+ vocabulary = ["Supabase"]', "+ [[replace]]"],
      status: "applied",
      error: null,
    },
  ],
  this_mac: {
    chip: "Apple M4 Max",
    memory_gb: 36,
    local_speech: true,
    local_cleanup: true,
  },
  chatgpt_account: "sam.rivera@example.com",
  live_draft_status: { model_ready: true, installed: true, enabled: false },
  benchmark_summary: {
    dictations: 42,
    edited: 7,
    confirmed: 1,
    guessed: 2,
    megabytes: 61.3,
  },
  benchmark_records: BENCHMARK,
  get_meeting_state: {
    recording: {
      id: "2026-09-30_10-00-00",
      mode: "call",
      status: "recording",
      started_at: Date.now() - 754_000,
      ended_at: null,
      mic: "MacBook Pro Microphone",
      system_error: null,
      tracks: [],
      title: "Weekly sync with Sam",
    },
    paused: null,
    elapsed_ms: 754_000,
    transcribing: null,
    live: false,
  },
  get_meeting_panel_state: { expanded: true },
  get_meeting_notes:
    "• Launch moved to Thursday\n• Sam to send the pricing sheet",
  meeting_level: 0.4,
  get_live_transcript: [
    {
      source: "system",
      start_ms: 12_000,
      end_ms: 30_000,
      text: "Morning! Shall we start with the launch date?",
      raw: null,
      speaker: null,
    },
    {
      source: "mic",
      start_ms: 31_000,
      end_ms: 52_000,
      text: "Yes, I think Thursday works better for everyone.",
      raw: null,
      speaker: null,
    },
  ],
  list_meetings: [
    {
      id: "2026-09-29_14-00-00",
      mode: "call",
      status: "recorded",
      started_at: Date.now() - 86_400_000,
      ended_at: Date.now() - 86_400_000 + 1_080_000,
      mic: "MacBook Pro Microphone",
      system_error: null,
      tracks: [],
      title: "Pricing review with Sam",
      transcript: "done",
      summary: null,
      speakers: { 100: "Sam Rivera", 101: "Priya" },
      app_speakers: {},
    },
  ],
  get_meeting_transcript: {
    complete: true,
    paragraphs: [
      {
        source: "system",
        start_ms: 4_000,
        end_ms: 18_000,
        text: "Thanks for joining. I'll share the pricing sheet in a second.",
        raw: null,
        speaker: 100,
      },
      {
        source: "mic",
        start_ms: 19_000,
        end_ms: 26_000,
        text: "Great, go ahead.",
        raw: null,
        speaker: null,
      },
      {
        source: "system",
        start_ms: 27_000,
        end_ms: 41_000,
        text: "Before that, can we check the launch date is still Thursday?",
        raw: null,
        speaker: 100,
      },
      {
        source: "system",
        start_ms: 42_000,
        end_ms: 55_000,
        text: "Yes, Thursday holds on our side.",
        raw: null,
        speaker: 101,
      },
    ],
  },
  get_meeting_summary: null,
  calendar_access: "not_determined",
  extension_status: {
    host_installed: true,
    heard_secs_ago: 40,
    app: "meet",
    extension_dir: "/Applications/Felix.app/Contents/Resources/resources/meet-extension",
  },
  get_meetings: [],
  get_available_microphones: [
    { index: "default", name: "MacBook Pro Microphone", is_default: true },
  ],
  get_available_output_devices: [
    { index: "default", name: "MacBook Pro Speakers", is_default: true },
  ],
  "plugin:app|version": "0.9.7",
  "plugin:os|platform": "macos",
  "plugin:os|os_type": "macos",
  "plugin:os|version": "26.0",
  "plugin:os|arch": "aarch64",
  "plugin:os|locale": "en-GB",
  "plugin:os|family": "unix",
  "plugin:event|listen": 1,
  "plugin:event|unlisten": null,
  "plugin:event|emit": null,
};

export function installTauriMock() {
  mockWindows(
    window.location.pathname.includes("overlay")
      ? "recording_overlay"
      : window.location.pathname.includes("meeting-panel")
        ? "meeting_panel"
        : "main",
  );
  mockConvertFileSrc("macos");
  mockIPC(
    (cmd) => {
      if (cmd in RESPONSES) return RESPONSES[cmd];
      if (cmd.startsWith("plugin:macos-permissions|check_")) return true;
      if (/^(get|list)_/.test(cmd)) return [];
      if (/^(check|is|has)_/.test(cmd)) return true;
      return null;
    },
    { shouldMockEvents: true },
  );
}

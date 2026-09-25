# Felix

A dictation app built on [**Handy**](https://github.com/cjpais/Handy): a
personal fork that turns it into an all-day dictation tool for macOS, with
vocabulary that sticks, text that gets tidied up before it's pasted,
voice commands, meeting notes and a voice assistant — while keeping Handy's core promise that your voice can
stay on your Mac.

> [!NOTE]
> This is an independent fork. It is not affiliated with or endorsed by
> Handy or its maintainers. Please don't file issues about this fork
> upstream.

## Standing on Handy's shoulders

Everything here is built on [Handy](https://handy.computer) by
[CJ Pais](https://github.com/cjpais) and its contributors. Handy did the
hard, unglamorous parts, and did them well: a fast Tauri app with
cross-platform audio capture, Silero voice activity detection, local
Whisper, Parakeet and friends with GPU acceleration, global shortcuts that
actually work, pasting into any app, model downloads, onboarding, a
translation system and a clean codebase that was a pleasure to build on.

This fork would not exist without it. If you want a free, open source,
private speech-to-text app, **use and support
[Handy](https://github.com/cjpais/Handy)** — it's the real thing.
Handy's original README is kept in [README.handy.md](README.handy.md).

## What this fork adds

A short tour; [docs/FEATURES.md](docs/FEATURES.md) has the details.

- **A staging pipeline instead of straight-to-paste.** Transcribe → your
  vocabulary's spellings → correction rules → voice commands → optional AI
  cleanup → per-app tone → paste.
- **Vocabulary that the speech model actually hears.** Custom words are
  fed to each model's native biasing (Whisper prompt, Qwen3-ASR context,
  decode-time keyword boosting for Cohere and Canary-Qwen), plus learned
  corrections, sound-alike fixes ("cloud" → "Claude" when you mean it) and
  words spelled out letter by letter.
- **Report a mistake.** Say or type what went wrong; a model proposes the
  rule that fixes it.
- **Writing cleanup.** Removes fillers, repeats and self-corrections,
  fixes punctuation and numbers — on a local Qwen model through Ollama
  (with the prompt cached while you speak), Apple Intelligence, or an API
  provider. Optional screen context and your own instructions.
- **Quiet speech.** Input gain and automatic gain control ahead of silence
  detection, and per-microphone detection thresholds, so whispering into a
  clip-on mic isn't thrown away as silence.
- **Voice commands.** "New line", "press enter", switching apps by voice.
- **Meetings.** Record, transcribe (locally, or with an OpenAI / Groq API
  key) and summarise meetings.
- **The Felix assistant** (optional, off by default): say its name and a
  ChatGPT model rewrites the focused field for you.
- **A redesign.** New information architecture and visual language —
  see [docs/DESIGN.md](docs/DESIGN.md).
- **Benchmark recording** for developers: keep raw audio, what was pasted,
  your edits and a ground-truth transcript, to tune gain, silence
  detection and models on real speech. See
  [docs/BENCHMARKING.md](docs/BENCHMARKING.md).

## Privacy

Speech recognition runs on your Mac by default. Anything that leaves it is
opt-in and labelled in the app (Models → "What runs where"): cloud
transcription and cleanup with your own API keys, the ChatGPT-backed
assistant, and screen context sent to a cloud cleanup model. Benchmark
recordings, history and logs stay in Handy's app-data folder.

## Platform

Developed and tested on **Apple Silicon Macs** only. The Windows and Linux
code from upstream is still here but untested with this fork's changes.

## Building

Same as upstream — see [BUILD.md](BUILD.md). In short:

```bash
bun install
mkdir -p src-tauri/resources/models
curl -o src-tauri/resources/models/silero_vad_v4.onnx https://blob.handy.computer/silero_vad_v4.onnx
bun run tauri dev
```

For design work without the backend: `bun run dev`, then open
`http://localhost:1420/?mock`.

## Credits

- [Handy](https://github.com/cjpais/Handy) by CJ Pais and contributors —
  the foundation of everything here.
- [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp),
  [ggml / whisper.cpp](https://github.com/ggml-org/whisper.cpp) and
  [transcribe-rs](https://github.com/cjpais/transcribe-rs) for local
  speech recognition.
- [Silero VAD](https://github.com/snakers4/silero-vad) for voice activity
  detection.
- [Tauri](https://tauri.app), [Ollama](https://ollama.com) and
  [llama.cpp](https://github.com/ggml-org/llama.cpp).
- Design inspiration from [Granola](https://granola.ai) and
  [Wispr Flow](https://wisprflow.ai).

## License

MIT, like Handy. The original copyright notice is kept in
[LICENSE](LICENSE), as the license requires. Vendored third-party code
keeps its own licenses.

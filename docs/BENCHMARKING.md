# Benchmarking on real speech

Settings like input gain, automatic gain control and the silence-detection
threshold are hard to judge by feel. This fork can record your real
dictations so they can be compared offline, on the same audio, with
different settings and models.

## Collecting (in the app)

Settings → For developers → **Benchmark recording**. Off by default.
While it's on, every dictation saves two files to
`<app data>/benchmark/`:

- `<id>.wav`: the microphone audio **before** gain and silence detection
  (16 kHz mono), so any setting can be replayed later.
- `<id>.json`: what the app did at the time — gain settings and what
  auto-gain had learned, the silence-detection backend and threshold, the
  microphone, the app you dictated into, the speech model, the raw
  transcript, what was pasted and, if you changed it within three minutes,
  your edited text (`edit`: `unchanged`, `edited` or `rewritten`).

Everything stays on your Mac. Recordings are private: never commit them.

## Ground truth

Settings → For developers → Ground truth → **Review** lists recordings
with a player. For each:

- **What you said**: type the words as spoken, fillers and repeats
  included. Saved as `ground_truth`.
- **AI guess** (optional, needs ChatGPT sign-in): transcribes the raw audio
  with OpenAI / Groq when you have API keys, then asks a ChatGPT model to
  reconcile those with the live transcript and your edit. It keeps every
  spoken word and fixes only obviously misheard ones. Saved as `guess`,
  with the transcripts it used. Check it before using it.

Preferred reference, in order: `ground_truth`, then `guess`, then
`edited` (only when `edit` is `edited` or `unchanged`).

## Replaying (outside the app)

`src-tauri/examples/vad_replay.rs` replays recordings through the app's
gain and Silero silence detection with several settings and shows what
each one keeps:

```bash
cd src-tauri
cargo run --release --example vad_replay -- \
  ~/Library/Application\ Support/com.pais.handy/benchmark/*.json \
  --out /tmp/vad-replay
```

```
app (0.30)     kept 10.5 of 15.5 s  ###################...................###########
sensitive 0.10 kept 15.5 of 15.5 s  #################################################
```

`#` is audio the speech model gets, `.` is audio dropped as silence (250 ms
per character). `--out` writes the kept audio per variant for listening or
transcribing.

Still to build: transcribing each variant and scoring word error rate
against the references, split by microphone and by whispered vs voiced
speech.

# What this fork changes

A map of what's been added on top of [Handy](https://github.com/cjpais/Handy),
by sidebar section. Anything that sends data off your Mac is off by default (the
assistant is on, but sends nothing until you sign in to ChatGPT) and listed under Models → "What runs where".

## Getting started

After the model is picked, a short tour where every step can be skipped: try
a dictation, optionally turn on the experimental live draft, teach a word (added to the vocabulary, then said three times
spoken and twice whispered), sign in to ChatGPT, and meet the assistant.
The dictation steps have a scratchpad beside them to watch it happen.
Settings → About → "Show the tour" opens it again. Teach a word is also on the Vocabulary page. The settings window now
opens at 860×680.

## The pipeline

Upstream Handy transcribes and pastes. Here every dictation goes through a
staging pipeline first:

1. **Transcribe**, with the model's native vocabulary biasing.
2. **Vocabulary spellings**: your words in their canonical form.
3. **Correction rules**: plain or regex replacements, learned or written.
4. **Voice commands**: "new line", "new paragraph", "press enter" and so on.
5. **AI cleanup** (optional, None / Light / Medium).
6. **Per-app tone**: casual in chat, more formal in email.
7. **Paste**, adapting spacing and capitalisation to the text around the
   cursor. When nothing is focused, the overlay shows the text with a Copy
   button instead.

## Dictation

- Shortcuts, microphone, language, pasting, sounds and silence detection.
- **Preferred microphones**: a priority list; the first one connected wins.
- **Input gain and automatic gain control** before silence detection, so
  quiet speech and weak mics reach the model. ⚠️ Whether automatic gain
  control improves accuracy hasn't been measured yet; it's being
  benchmarked.
- **Per-microphone silence thresholds** (`microphone_vad_thresholds`, no UI
  yet): a close clip-on mic used for whispering can run a much more
  sensitive Silero threshold than a laptop mic at arm's length. ⚠️
  Values are provisional (set from a few recordings); too sensitive can
  produce words from breath or noise.

- **Quiet-speech safety net**: after each dictation the raw audio is run
  through a much more sensitive silence threshold (a few milliseconds). If
  that keeps clearly more audio, it's transcribed too, and used only when it
  contains the first transcript plus real new words, not the "Thank you."
  models invent from breathing.

**Live draft** (experimental, off by default: the Tiny model's draft is
rough, some apps such as WhatsApp show it in a floating bubble instead of
inline, and the second model costs a little extra processing): while you talk, Moonshine
Streaming Tiny transcribes alongside the main model and its rough text shows
as underlined marked text in the focused field, through Felix Draft, a small
input method (`src-tauri/draft-ime/`, built by `scripts/build-draft-ime.sh`,
installed to ~/Library/Input Methods on first use; you add it once under
Keyboard → Input Sources). Felix switches to it for each dictation and back
afterwards. The draft is cleared the moment recording stops, before anything
reads the field, and the main model's text is pasted as before. Skipped in
secure input and when text is selected.

## Vocabulary

- **Native biasing** per model: Whisper's initial prompt, Qwen3-ASR's
  recognition context, and decode-time keyword boosting added to
  transcribe.cpp for Cohere and Canary-Qwen.
- **Learned rules**: corrections, sound-alikes ("cloud" → "Claude" only in
  the right context) and names that depend on the app you're in.
- **Spelled-out words**: "Rivera, R I V E R A" becomes "Rivera".
- **Report a mistake**, by voice or text; a model writes the rule and tests
  it. A fix that passes its tests is applied straight away (with Undo and
  "Show change"), so you can carry on while it works; anything else waits
  for a look.
- **Portable rules**: everything lives in a commented `rules.toml` that
  explains itself to AI agents and other apps; Vocabulary → Export saves it
  as TOML or JSON (with the words taught by voice) to load elsewhere.

## Writing

- **Cleanup** of fillers, repeats, self-corrections, punctuation and
  numbers, with guards that reject rewrites that add or drop content.
- **Providers**: a local Qwen model via Ollama (one-click setup; the prompt
  is cached while you speak so only the transcript is processed after),
  Apple Intelligence, or an API provider with your key.
- **Your instructions**, **screen context** (optional) and **tone by app**.
- **Coding agents** (Claude, Codex, Cursor, VS Code, Zed, Xcode, terminals,
  claude.ai, chatgpt.com): the same words and tone, laid out to check before
  pressing Enter: several requests become a bulleted list and each sentence
  gets its own line. Always goes through cleanup.
- **Prompt shortcut**: a separate shortcut that sends the dictation to the
  model as an instruction.

Long dictations with Cohere Transcribe or Canary-Qwen 2.5B are transcribed
in pieces of up to 30 s, cut at pauses: run whole, both dropped about half
of a 114 s dictation. Qwen3-ASR and Whisper were fine whole and are
unchanged (`SPLIT_LONG_AUDIO_ARCHS`). Meetings go through the same path.

## Voice commands

Trigger phrases at the end of a dictation, "new line", and spoken symbols.

## Felix

A voice assistant, on by default but inactive until you sign in to ChatGPT. Say its name and it takes
the dictation and the focused field to a ChatGPT model, which inserts,
replaces the selection or rewrites the field. "Felix, go to Codex" brings
that app to the front, and "Felix, it keeps writing cloud instead of Claude"
fixes the dictation rules.

⚠️ Work in progress. The "act on your Mac" (agent) mode (starting Claude Code
sessions, running tasks in other apps through Codex CLI and Cua Driver) is
experimental and not hardened. While a task runs, a card shows each step in
plain words; closing it stops the task. Every driver call goes through one
gate in Felix: it asks before Felix first uses an app (Always allow / Allow
this time / Deny / Always deny; saved choices can be removed on the Felix
page), never uses password managers or System Settings, asks
every time for terminals, and asks for a confirmation (button or a spoken
"yes") before anything that sends, posts, deletes or buys. Recent tasks are
listed step by step on the Felix page. Fast mode (Simple Jev picks each
step) is very experimental and off by default. The bundled Cua driver is pinned: its update check and
telemetry are off. ChatGPT sign-in reuses the Codex CLI's public
OAuth client, which OpenAI doesn't officially support for other apps.

## Meetings

⚠️ Work in progress: unfinished and still changing.

Record meetings (microphone and system audio), transcribe them locally or
with an OpenAI or Groq API key, identify speakers and write summaries.
Meetings never interrupt or change dictation.

## Models

What runs where, accounts and API keys, speech models, cleanup models and
memory (unloading models when idle).

## Settings → For developers

**Benchmark recording** and **ground truth** — see
[BENCHMARKING.md](BENCHMARKING.md).

## Design

A new look and information architecture, documented in
[DESIGN.md](DESIGN.md).

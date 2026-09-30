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
- **Pasting survives app updates**: when nothing that takes text seems
  focused, Felix shows the dictation instead of pasting, but only in an
  app version that has shown it a focused text box before (Apple's apps
  always count). After an update (or a new bundle id, as ChatGPT's
  2026-09 update did) it pastes as normal until it's seen one. The card's
  "Paste anyway" pastes and always pastes in that app from then on
  (`paste_apps.json`).
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

**Live draft** (experimental, off by default; the second model costs a
little extra processing): while you talk, Moonshine Streaming Tiny
transcribes alongside the main model and its rough text is shown, then the
main model's text is pasted as usual. Two styles:

- **Bubble** (`draft_bubble.rs`): a click-through bubble styled like the
  recording pill that follows the text cursor, or rests on the pill.
- **Inline**: underlined marked text in the focused field, through Felix
  Draft, a small input method (`src-tauri/draft-ime/`, built by
  `scripts/build-draft-ime.sh`; you add it once under Keyboard → Input
  Sources). Some apps, such as WhatsApp, show it in a floating box instead.

The draft only shows words two guesses in a row agree on, so a word the
model is still unsure of doesn't flicker in and out; shown words stay until
two guesses agree on something else. When the draft model gets stuck
repeating a word or phrase, the repeat is shown once until it recovers. Skipped with secure input on (and, inline,
when text is selected).

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
  numbers, with guards that reject rewrites that add or drop content. Em
  dashes the model adds (that you didn't say) become commas.
- **Providers**: a local Qwen model via Ollama (one-click setup; the prompt
  is cached while you speak so only the transcript is processed after),
  Apple Intelligence, or an API provider with your key.
- **Your instructions**, **screen context** (optional) and **tone by app**.
- **Coding agents** (Claude, Codex, Cursor, VS Code, Zed, Xcode, claude.ai,
  chatgpt.com; terminals are left out, so a command stays as spoken): the same words and tone, laid out to check before
  pressing Enter: several requests become a bulleted list and each sentence
  gets its own line. Always goes through cleanup.
- **Prompt shortcut**: a separate shortcut that sends the dictation to the
  model as an instruction.

Long dictations with Cohere Transcribe or Canary-Qwen 2.5B are transcribed
in pieces of up to 30 s, cut at pauses: run whole, both dropped about half
of a 114 s dictation. Qwen3-ASR and Whisper were fine whole and are
unchanged (`SPLIT_LONG_AUDIO_ARCHS`). Meetings go through the same path.
For every other model there's a safety net: a dictation over 45 s with
fewer than 100 words a minute is run again in pieces, and the pieces are
used only if they hold at least 10% more words.

## History

Each dictation keeps its audio (per the retention setting), the raw
transcript, what was pasted, the app (and site) it went into, its group
under Tone by app, and what cleanup was given from the screen. Retry
re-transcribes the audio and cleans it up with that same app and screen
context; it never runs the assistant or answers Felix's card.

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

- **Recording that recovers**: a mic or system-audio tap that stops
  delivering, a change of the default mic or output, or five minutes of
  silence from the tap on a call reopens the device; the gap is kept as
  silence. If the chosen mic disappears, recording carries on with the
  default one.
- **Mic volume held steady**: when a call app (Zoom, Teams) moves the Mac's
  input volume mid-meeting, the recording is scaled back to the level it
  started at (within ±18 dB). Mics without a volume control are left alone.
- **In-person gain**: the loudest 5% of the last 10 s is aimed at a steady
  level (at most ×8, changing slowly), and the silence detection hears the
  same, so quiet and distant voices are found. On test clips it cut the word
  error rate from 70% (the old per-stretch levelling) to 49%.
- **Summaries** keep very short meetings short, follow up on the last
  meeting with the same name (its overview and action items), and use
  British spelling when the Mac is set to British English.
- **Right speed on AirPods**: the system-audio tap reads the rate the
  output really runs at (AirPods drop to 24 kHz in a call while the tap
  says 48 kHz), and every track checks the rate it measures, so a call is
  never recorded at double speed with gaps.
- **Languages**: Settings → Meetings → Meeting languages (several allowed;
  none means Automatic, which listens to eight stretches of the meeting).
  On this Mac the meeting goes to the most accurate downloaded model that
  knows every language, which may not be the dictation model; the cloud
  gets the language, or a note that the speakers switch between them.
- **Side panel**: Pause and Resume (the files close; Stop transcribes),
  Notes and Transcript tabs with one scroll, bullet lists in notes ("- ",
  Enter, Tab), a running rough transcript (the cloud, or the dictation
  model in 15 s pieces only when dictation isn't using it), snapping to a
  side of the screen when dropped near it, and a bouncy open and fold.
- **Rooms on a call**: when the call app names one person for several
  voices that each talk for 20 s or more, they show as "Name (1)",
  "Name (2)" instead of all as one. When speaker detection hears only one voice
  on the other side (a muddy line) but the call app showed several names,
  the names split it instead. Names are read from the call window, not the
  app's menus.
- **Your voice in person** (groundwork, not switched on): given a voiceprint
  for the mic, the in-person speaker that clearly matches it becomes "Me".
  Nothing records voiceprints yet; they'll be one per mic, and never made
  from dictation mics like a DJI clip mic.

## Models

What runs where, accounts and API keys, speech models, cleanup models and
memory (unloading models when idle).

## Settings → For developers

**Benchmark recording** and **ground truth** — see
[BENCHMARKING.md](BENCHMARKING.md).

## Design

A new look and information architecture, documented in
[DESIGN.md](DESIGN.md).

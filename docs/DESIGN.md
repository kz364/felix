# Handy design guide

Handy is a tool you talk to all day and look at for a few seconds. The
settings window should feel like a well-kept notebook: warm paper, dark
ink, one colour for "on", and nothing that shouts. The references are
Granola (cream canvas, olive accent, serif headlines, "calm, with energy
underneath") and Wispr Flow (warm off-white, soft pill controls, short
friendly copy, a small sidebar of nouns).

Tokens live in `src/styles/theme.css` and are registered as Tailwind
colours in `src/App.css`. Never use Tailwind's stock palette (`red-400`,
`green-500`, `gray-100`, `white` …) in components; use the tokens below.

## Colour

| Token        | Light     | Dark      | Use                                                   |
| ------------ | --------- | --------- | ----------------------------------------------------- |
| `background` | `#F7F6F1` | `#1B1B19` | Window canvas (content area)                          |
| `sunken`     | `#EFEEE7` | `#141412` | Sidebar, footer, inset wells, code/log blocks         |
| `surface`    | `#FFFFFE` | `#242421` | Cards, menus, inputs, dialogs                         |
| `text`       | `#23221E` | `#EDEBE4` | Ink. Secondary text is `text-text/65`, tertiary `/45` |
| `stone`      | `#8B887E` | `#8F8C83` | Warm neutral: hairlines (`/20`), fills (`/10`), icons |
| `accent`     | `#4E6A12` | `#B4CC62` | The one colour for "on": toggles, focus, selection    |
| `on-accent`  | `#FBFBF5` | `#1B1B19` | Text on a solid accent fill                           |
| `highlight`  | `#F2C24B` | `#E8B93F` | Highlighter yellow: recording, "new", tiny moments    |
| `success`    | `#3F7A3A` | `#8FCB84` | Done, passed, saved                                   |
| `warning`    | `#A86A12` | `#E9B454` | Needs attention, not broken                           |
| `error`      | `#C2462B` | `#F08A6E` | Failed, destructive                                   |

Rules:

- One accent. It marks state (on, selected, focused, primary action), never
  decoration. Don't tint whole cards with it; `accent/10` is the most fill
  you should see.
- Hairlines are `border-stone/20`; dividers inside cards `divide-stone/15`.
- Status colours appear as text or as a `/10` fill with matching text, never
  as a solid slab (except a single destructive confirm button).
- `logo-primary` and `logo-stroke` exist only for the SVG logo (they point
  at `accent` and `text`). Don't use them in components.

## Type

- **Display serif** (`font-display`): the system serif — New York on macOS
  (`ui-serif`), falling back to Iowan Old Style, Charter, Georgia. Used only
  for page titles, empty-state headlines and big numbers. Regular weight,
  slightly tight tracking. Nothing bundled or downloaded.
- **UI sans** (`font-sans`): the system UI font (SF Pro). Everything else.
- **Mono** (`font-mono`): SF Mono for paths, shortcuts, rules and logs.

| Role        | Class                                    |
| ----------- | ---------------------------------------- |
| Page title  | `font-display text-[28px] leading-tight` |
| Page intro  | `text-sm text-text/65`, max ~60ch        |
| Group label | `text-[13px] font-medium text-text/70`   |
| Row title   | `text-sm font-medium`                    |
| Row help    | `text-[13px] text-text/60`               |
| Fine print  | `text-xs text-text/50`                   |

Sentence case everywhere ("Launch at login", not "Launch At Login").
No all-caps labels.

## Shape and space

- Radius: controls 8px (`rounded-lg`), cards and menus 12px (`rounded-xl`),
  segmented controls and badges fully round (`rounded-full`).
- Cards: `bg-surface border border-stone/20 rounded-xl`, no shadow. Menus and
  popovers add `shadow-lg shadow-black/10`.
- Page column: `max-w-2xl`, 32px top padding, 24px between groups.
- Rows inside a card: 16px horizontal padding, min height 52px.
- Icons: lucide, 14–16px in rows, 16px at stroke 1.75 in the sidebar.

## Components

- **Sidebar**: `bg-sunken`, 208px, wordmark at the top. Main nouns first,
  then _Models_ and _Settings_ pinned to the bottom. The active item is a
  raised pill (`bg-surface` + hairline), not a coloured slab.
- **Page header** (`PageHeader`): serif title plus one-sentence intro.
  Every page has one.
- **SettingsGroup**: small label above a card of rows. Optional
  one-line description under the label.
- **SettingContainer / row**: title left, control right; help text as a
  quiet `(i)` tooltip or inline muted line.
- **Toggle**: 36×20 track, `stone/30` off, `accent` on, white knob.
- **Button**: `primary` is solid ink (`bg-text text-background`),
  `secondary` is surface with hairline, `ghost` is text only, `danger` is
  error text on hover-tinted ground. Heights 28 (sm) / 32 (md) / 36 (lg).
- **Input, Textarea, Dropdown**: `bg-surface`, hairline border, normal weight
  text, accent focus ring (`focus:border-accent focus:ring-[3px]
focus:ring-accent/20`).
- **Segmented control**: pill track in `sunken`, active segment `surface`
  with a hairline — the Wispr pattern.
- **Alert**: `/10` status fill, status-coloured icon, ink text.
- **Badge**: small round pill; `accent/10` + accent text, or `stone/15`.

## Motion

150 ms colour/opacity transitions; nothing bounces. Respect
`prefers-reduced-motion`.

## Voice

Handy talks like a thoughtful friend who happens to be good with
computers.

- Short, plain sentences. Say what happens, not how it's implemented:
  "Handy tidies up your words before pasting" beats "Post-processing
  pipeline".
- Second person, active voice. "Your words", "your Mac".
- Nouns for places (Dictation, History, Vocabulary, Writing), verbs for
  buttons (Save, Add word, Sign in).
- No jargon in titles: _Writing_ not "Post Process", _Voice commands_ not
  "Voice Control Mode", _Skip silence_ not "Voice Activity Detection",
  _Free up memory_ not "Unload Model".
- Sentence case, no exclamation marks, no "please", no "simply".
- Numbers and names stay exact (model names, shortcuts, file paths).

## Information architecture

| Sidebar             | What lives there                                                                                      |
| ------------------- | ----------------------------------------------------------------------------------------------------- |
| Dictation           | Shortcuts · microphone · language · pasting · sounds · listening (silence detection)                  |
| History             | Past dictations · how long to keep them                                                               |
| Vocabulary          | Report a mistake · what Handy has learned · your reports                                              |
| Writing             | Cleanup on/off and level · your instructions · screen context · tone by app · prompt shortcut         |
| Voice commands      | Trigger phrases · switching apps by voice                                                             |
| Felix               | The assistant and acting on your Mac                                                                  |
| Meetings            | Recorder, recordings, meeting settings                                                                |
| _(bottom)_ Models   | What runs where · accounts & keys · speech models · cleanup models · memory                           |
| _(bottom)_ Settings | On your Mac (login, background, menu bar, overlay) · look and language · about Handy · for developers |
| _(hidden)_ Debug    | Cmd+Shift+D                                                                                           |

### What moved where (September 2026 reorganisation)

- **General** became **Dictation**, and took the pasting and silence
  settings from **Advanced**.
- **Post Process** and **Style** became **Writing**; filler-word removal
  moved there too.
- **Voice Control** became **Voice commands**.
- **Advanced** and **About** were merged into **Settings**; history
  retention moved to **History**, and "unload model" to **Models → Memory**.

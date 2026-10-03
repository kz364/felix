import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowLeft,
  Calendar,
  ChevronRight,
  Clock,
  Copy,
  FolderOpen,
  Loader2,
  MessageCircleQuestion,
  Mic,
  Monitor,
  Pause,
  Pencil,
  Play,
  RotateCcw,
  Send,
  Settings2,
  Sparkles,
  Square,
  Trash2,
  Users,
  Video,
} from "lucide-react";
import {
  type ParagraphPart,
  commands,
  type LiveQuestion,
  type MeetingInfo,
  type MeetingVouch,
  type Vouch,
  type MeetingMode,
  type MeetingState,
  type MeetingTranscript,
  type Paragraph,
  type Summary,
  type TranscribeProgress,
} from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { PageHeader } from "../ui/PageHeader";
import { openSection } from "@/lib/navigation";
import {
  clock,
  LiveBars,
  LiveHelp,
  NotesEditor,
  TitleEditor,
  useMeetingTitle,
} from "./live";

/** A small rounded detail, like Granola's "Today · 4" chips. */
const Chip: React.FC<{
  icon?: React.ReactNode;
  tone?: "default" | "warning" | "accent";
  children: React.ReactNode;
}> = ({ icon, tone = "default", children }) => (
  <span
    className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full border text-xs ${
      tone === "warning"
        ? "border-warning/40 text-warning bg-warning/10"
        : tone === "accent"
          ? "border-accent/40 text-text/80 bg-accent/10"
          : "border-stone/25 text-text/70 bg-stone/5"
    }`}
  >
    {icon}
    {children}
  </span>
);

const modeIcon = (mode: MeetingMode, size = "w-3.5 h-3.5") =>
  mode === "call" ? <Video className={size} /> : <Users className={size} />;

/** Plays both tracks together, from any point. */
const useMeetingPlayer = (meeting: MeetingInfo) => {
  const audios = useRef<HTMLAudioElement[]>([]);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const duration = Math.max(0, ...meeting.tracks.map((tr) => tr.seconds));

  const load = async () => {
    if (audios.current.length > 0) return true;
    const els: HTMLAudioElement[] = [];
    for (const track of meeting.tracks) {
      const result = await commands.meetingTrackPath(meeting.id, track.file);
      if (result.status === "error") {
        toast.error(result.error);
        continue;
      }
      const audio = new Audio(convertFileSrc(result.data));
      audio.preload = "auto";
      els.push(audio);
    }
    const lead = els[0];
    lead?.addEventListener("timeupdate", () => setTime(lead.currentTime));
    lead?.addEventListener("ended", () => setPlaying(false));
    audios.current = els;
    return els.length > 0;
  };

  const playFrom = async (seconds: number) => {
    if (!(await load())) return;
    for (const a of audios.current) a.currentTime = seconds;
    setTime(seconds);
    await Promise.all(audios.current.map((a) => a.play().catch(() => {})));
    setPlaying(true);
  };

  const pause = () => {
    for (const a of audios.current) a.pause();
    setPlaying(false);
  };

  const seek = (seconds: number) => {
    setTime(seconds);
    for (const a of audios.current) a.currentTime = seconds;
  };

  useEffect(
    () => () => {
      for (const a of audios.current) {
        a.pause();
        a.removeAttribute("src");
      }
    },
    [],
  );

  return { playing, time, duration, playFrom, pause, seek };
};

type Player = ReturnType<typeof useMeetingPlayer>;

const PlayerBar: React.FC<{ player: Player }> = ({ player }) => {
  const { t } = useTranslation();
  const percent =
    player.duration > 0 ? (player.time / player.duration) * 100 : 0;
  return (
    <div className="flex items-center gap-3 rounded-full border border-stone/20 bg-surface/90 backdrop-blur px-2 py-1.5">
      <button
        onClick={() =>
          player.playing ? player.pause() : player.playFrom(player.time)
        }
        aria-label={
          player.playing
            ? t("meetings.player.pause")
            : t("meetings.player.play")
        }
        className="w-8 h-8 shrink-0 rounded-full bg-text text-background flex items-center justify-center cursor-pointer hover:opacity-90"
      >
        {player.playing ? (
          <Pause className="w-3.5 h-3.5 fill-current" />
        ) : (
          <Play className="w-3.5 h-3.5 fill-current ms-0.5" />
        )}
      </button>
      <span className="text-xs text-text/60 tabular-nums w-10 text-end">
        {clock(player.time * 1000)}
      </span>
      <input
        type="range"
        min={0}
        max={player.duration || 0}
        step={0.1}
        value={player.time}
        onChange={(e) => player.seek(parseFloat(e.target.value))}
        className="flex-1 h-1 rounded-lg appearance-none cursor-pointer focus:outline-none"
        style={{
          background: `linear-gradient(to right, #FAA2CA 0%, #FAA2CA ${percent}%, rgba(128,128,128,0.2) ${percent}%, rgba(128,128,128,0.2) 100%)`,
        }}
      />
      <span className="text-xs text-text/60 tabular-nums w-10 pe-2">
        {clock(player.duration * 1000)}
      </span>
    </div>
  );
};

/** Voices on the other side of a call are numbered from here. */
const SYSTEM_SPEAKERS = 100;
/** The user's own voice, matched by their voiceprint (in person). */
const ME = 4294967295;

/** "Speaker 2" or "Them 1", or the name the user (or the call app) gave. */
const useSpeakerName = () => {
  const { t } = useTranslation();
  return (meeting: MeetingInfo, n: number) =>
    meeting.speakers?.[n] ??
    meeting.app_speakers?.[n] ??
    (n === ME
      ? t("meetings.speaker.me")
      : n >= SYSTEM_SPEAKERS
        ? t("meetings.speaker.themNumbered", { n: n - SYSTEM_SPEAKERS + 1 })
        : t("meetings.speaker.numbered", { n: n + 1 }));
};

/** Who said a paragraph, as the transcript labels it. */
const useWho = () => {
  const { t } = useTranslation();
  const nameOf = useSpeakerName();
  return (meeting: MeetingInfo, p: Paragraph) =>
    p.speaker !== null
      ? nameOf(meeting, p.speaker)
      : meeting.mode === "call"
        ? p.source === "mic"
          ? t("meetings.speaker.me")
          : t("meetings.speaker.them")
        : null;
};

/** Someone in the meeting a paragraph can be given to. */
type Person = { n: number; label: string };

/** Everyone the transcript names, in order of first word, plus "Me". */
const usePeople = () => {
  const nameOf = useSpeakerName();
  return (meeting: MeetingInfo, paragraphs: Paragraph[]): Person[] => {
    const out: Person[] = [];
    const add = (n: number) => {
      const label = nameOf(meeting, n);
      if (!out.some((p) => p.n === n || p.label === label)) {
        out.push({ n, label });
      }
    };
    if (meeting.mode === "call") add(ME);
    for (const p of paragraphs) if (p.speaker !== null) add(p.speaker);
    for (const k of Object.keys(meeting.speakers ?? {})) add(Number(k));
    return out;
  };
};

const SPEAKER_COLORS = [
  "text-accent",
  "text-accent",
  "text-success",
  "text-warning",
  "text-violet-500",
];

/** Who said a paragraph. Click to give just this paragraph to someone
 * else, or to rename the voice everywhere. */
const SpeakerLabel: React.FC<{
  meeting: MeetingInfo;
  p: Paragraph;
  label: string;
  people: Person[];
  editable: boolean;
  doubt?: string;
  onChanged: () => void;
}> = ({ meeting, p, label, people, editable, doubt, onChanged }) => {
  const { t } = useTranslation();
  // Splitting: the paragraph's pieces, then the piece the new speaker
  // starts at. Merging: picking who this voice really is.
  const [parts, setParts] = useState<ParagraphPart[] | null>(null);
  const [splitAt, setSplitAt] = useState<number | null>(null);
  const [merging, setMerging] = useState(false);
  // Where the menu opens: fixed to the window, so the transcript's
  // scrolling box can't cut it off; above the name near the bottom edge.
  const [open, setOpen] = useState<React.CSSProperties | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [someoneElse, setSomeoneElse] = useState<string | null>(null);
  const menuRef = useRef<HTMLSpanElement>(null);
  const color =
    p.speaker !== null && p.speaker !== ME
      ? SPEAKER_COLORS[p.speaker % SPEAKER_COLORS.length]
      : p.source === "mic"
        ? "text-text"
        : "text-accent";
  const numbered = p.speaker !== null && p.speaker !== ME;

  useEffect(() => {
    if (!open) return;
    const close = (e: Event) => {
      if (e.type === "scroll" || !menuRef.current?.contains(e.target as Node)) {
        setOpen(null);
        setSomeoneElse(null);
        setParts(null);
        setSplitAt(null);
        setMerging(false);
      }
    };
    document.addEventListener("mousedown", close);
    window.addEventListener("scroll", close, true);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [open]);

  const toggle = (button: HTMLElement) => {
    if (open) {
      setOpen(null);
      return;
    }
    const r = button.getBoundingClientRect();
    const below = window.innerHeight - r.bottom > 260;
    setOpen(
      below
        ? { left: r.left, top: r.bottom + 4 }
        : { left: r.left, bottom: window.innerHeight - r.top + 4 },
    );
  };

  const close = () => {
    setOpen(null);
    setSomeoneElse(null);
    setParts(null);
    setSplitAt(null);
    setMerging(false);
  };

  const assign = async (speaker: number | null, name: string | null) => {
    // After picking where to split, the person is for the rest from there.
    const from = splitAt ?? p.start_ms;
    close();
    const result = await commands.setParagraphSpeaker(
      meeting.id,
      p.source,
      from,
      speaker,
      name,
    );
    if (result.status === "error") toast.error(result.error);
    onChanged();
  };

  const startSplit = async () => {
    const result = await commands.paragraphParts(
      meeting.id,
      p.source,
      p.start_ms,
    );
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setParts(result.data);
  };

  const merge = async (into: number) => {
    close();
    const result = await commands.mergeMeetingVoices(
      meeting.id,
      p.speaker as number,
      into,
    );
    if (result.status === "error") toast.error(result.error);
    onChanged();
  };

  const rename = async () => {
    const name = renaming?.trim();
    setRenaming(null);
    if (!numbered || name === undefined || name === label) return;
    const result = await commands.renameMeetingSpeaker(
      meeting.id,
      p.speaker as number,
      name,
    );
    if (result.status === "error") toast.error(result.error);
    onChanged();
  };

  if (renaming !== null) {
    return (
      <input
        autoFocus
        value={renaming}
        onChange={(e) => setRenaming(e.target.value)}
        onBlur={rename}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") setRenaming(null);
        }}
        className={`me-1.5 w-32 bg-transparent border-b border-accent/60 outline-none font-medium ${color}`}
      />
    );
  }
  const mark = doubt ? (
    <span
      title={t(`meetings.speaker.doubt.${doubt}`, {
        defaultValue: t("meetings.speaker.doubt.close"),
      })}
      className="-ms-1 me-1.5 cursor-help text-xs text-text/45"
    >
      ?
    </span>
  ) : null;
  if (!editable) {
    return (
      <>
        <span className={`me-1.5 font-medium ${color}`}>{label}</span>
        {mark}
      </>
    );
  }
  const item = "rounded px-2 py-1 text-start hover:bg-stone/10 cursor-pointer";
  const muted = `${item} text-text/70`;
  return (
    <span ref={menuRef}>
      <button
        onClick={(e) => toggle(e.currentTarget)}
        title={t("meetings.speaker.whoSaid")}
        className={`me-1.5 font-medium cursor-pointer hover:underline decoration-dotted ${color}`}
      >
        {label}
      </button>
      {open && (
        <span
          style={open}
          className="fixed z-50 flex w-56 flex-col rounded-lg border border-stone/20 bg-background p-1 text-sm shadow-lg"
        >
          {parts !== null && splitAt === null ? (
            <>
              <span className="px-2 py-1 text-xs text-text/50">
                {t("meetings.speaker.splitFrom")}
              </span>
              {parts.length < 2 && (
                <span className="px-2 py-1 text-text/50">
                  {t("meetings.speaker.cantSplit")}
                </span>
              )}
              {parts.slice(1).map((part) => (
                <button
                  key={part.start_ms}
                  onClick={() => setSplitAt(part.start_ms)}
                  className={`${item} line-clamp-2`}
                >
                  {part.text}
                </button>
              ))}
            </>
          ) : merging ? (
            <>
              <span className="px-2 py-1 text-xs text-text/50">
                {t("meetings.speaker.samePersonAs", { name: label })}
              </span>
              {people
                .filter((person) => person.n !== p.speaker && person.n !== ME)
                .map((person) => (
                  <button
                    key={person.n}
                    onClick={() => merge(person.n)}
                    className={item}
                  >
                    {person.label}
                  </button>
                ))}
            </>
          ) : (
            <>
              <span className="px-2 py-1 text-xs text-text/50">
                {splitAt !== null
                  ? t("meetings.speaker.whoSaidRest")
                  : t("meetings.speaker.whoSaid")}
              </span>
              {people
                .filter((person) => person.label !== label)
                .map((person) => (
                  <button
                    key={person.n}
                    onClick={() => assign(person.n, null)}
                    className="rounded px-2 py-1 text-start hover:bg-stone/10 cursor-pointer"
                  >
                    {person.label}
                  </button>
                ))}
              {someoneElse === null ? (
                <button
                  onClick={() => setSomeoneElse("")}
                  className="rounded px-2 py-1 text-start text-text/70 hover:bg-stone/10 cursor-pointer"
                >
                  {t("meetings.speaker.someoneElse")}
                </button>
              ) : (
                <input
                  autoFocus
                  value={someoneElse}
                  placeholder={t("meetings.speaker.namePlaceholder")}
                  onChange={(e) => setSomeoneElse(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && someoneElse.trim()) {
                      assign(null, someoneElse.trim());
                    }
                    if (e.key === "Escape") setSomeoneElse(null);
                  }}
                  className="mx-1 my-0.5 rounded border border-accent/50 bg-background px-1.5 py-0.5 outline-none"
                />
              )}
              {numbered && splitAt === null && (
                <>
                  <span className="my-1 h-px bg-stone/20" />
                  <button
                    onClick={() => {
                      setOpen(null);
                      setRenaming(label);
                    }}
                    className="rounded px-2 py-1 text-start text-text/70 hover:bg-stone/10 cursor-pointer"
                  >
                    {t("meetings.speaker.renameEverywhere", { name: label })}
                  </button>
                  {people.some((x) => x.n !== p.speaker && x.n !== ME) && (
                    <button onClick={() => setMerging(true)} className={muted}>
                      {t("meetings.speaker.samePerson", { name: label })}
                    </button>
                  )}
                </>
              )}
              {splitAt === null && (
                <button onClick={startSplit} className={muted}>
                  {t("meetings.speaker.split")}
                </button>
              )}
            </>
          )}
        </span>
      )}
      {mark}
    </span>
  );
};

const TranscriptParagraph: React.FC<{
  p: Paragraph;
  meeting: MeetingInfo;
  showOriginal: boolean;
  active: boolean;
  editable: boolean;
  people: Person[];
  doubt?: string;
  onPlay: () => void;
  onChanged: () => void;
}> = ({
  p,
  meeting,
  showOriginal,
  active,
  editable,
  people,
  doubt,
  onPlay,
  onChanged,
}) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState<string | null>(null);
  const who = useWho();
  const speaker = who(meeting, p);
  const text = showOriginal && p.raw ? p.raw : p.text;

  const save = async () => {
    const edited = editing?.trim();
    setEditing(null);
    if (edited === undefined || edited === p.text) return;
    const result = await commands.editMeetingParagraph(
      meeting.id,
      p.source,
      p.start_ms,
      edited,
    );
    if (result.status === "error") toast.error(result.error);
    onChanged();
  };

  return (
    <div
      className={`group grid grid-cols-[3.5rem_1fr] gap-3 rounded-lg px-2 py-2 transition-colors ${
        active ? "bg-accent/10" : "hover:bg-stone/5"
      }`}
    >
      <button
        onClick={onPlay}
        title={t("meetings.transcript.playFrom", { time: clock(p.start_ms) })}
        className="self-start mt-0.5 text-xs tabular-nums text-text/45 hover:text-accent cursor-pointer text-start"
      >
        {clock(p.start_ms)}
      </button>
      <p className="text-sm leading-relaxed">
        {speaker && (
          <SpeakerLabel
            meeting={meeting}
            p={p}
            label={speaker}
            people={people}
            editable={editable}
            doubt={doubt}
            onChanged={onChanged}
          />
        )}
        {editing !== null ? (
          <textarea
            autoFocus
            value={editing}
            rows={Math.max(2, Math.ceil(editing.length / 80))}
            onChange={(e) => setEditing(e.target.value)}
            onBlur={save}
            onKeyDown={(e) => {
              if (e.key === "Escape") setEditing(null);
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.currentTarget.blur();
              }
            }}
            className="mt-1 block w-full resize-y rounded-md border border-accent/50 bg-background px-2 py-1 text-sm leading-relaxed outline-none"
          />
        ) : (
          <span className="text-text/85">{text}</span>
        )}
        {editable && editing === null && (
          <button
            onClick={() => setEditing(p.text)}
            title={t("meetings.transcript.edit")}
            className="ms-1.5 inline-flex align-middle text-text/35 opacity-0 group-hover:opacity-100 hover:text-accent cursor-pointer"
          >
            <Pencil className="w-3 h-3" />
          </button>
        )}
      </p>
    </div>
  );
};

/** Seconds as "25 min" or "40 s". */
const speechLength = (secs: number) =>
  secs >= 90 ? `${Math.round(secs / 60)} min` : `${secs} s`;

const learnedList = (learned: Partial<Record<string, number>>) =>
  Object.entries(learned)
    .map(([name, secs]) => `${name} (${speechLength(secs ?? 0)})`)
    .join(", ");

/**
 * Say a call's voices are clean enough to learn from: a 1-on-1 with someone
 * (the whole call side is them) or every call voice named right. Felix
 * otherwise only learns people from what the call app marked one at a time.
 */
const VouchVoices: React.FC<{ meeting: MeetingInfo; onClose: () => void }> = ({
  meeting,
  onClose,
}) => {
  const { t } = useTranslation();
  const [state, setState] = useState<MeetingVouch | null>(null);
  const [name, setName] = useState("");
  const [working, setWorking] = useState(false);

  useEffect(() => {
    commands.meetingVouch(meeting.id).then((result) => {
      if (result.status === "error") {
        toast.error(result.error);
        onClose();
        return;
      }
      setState(result.data);
      setName(result.data.vouch.one_on_one ?? result.data.suggested ?? "");
    });
  }, [meeting.id, onClose]);

  const save = async (vouch: Vouch) => {
    setWorking(true);
    const result = await commands.vouchMeeting(meeting.id, vouch);
    setWorking(false);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    const learned = learnedList(result.data);
    if (vouch.one_on_one || vouch.trusted) {
      toast.success(
        learned
          ? t("meetings.vouch.learned", { people: learned })
          : t("meetings.vouch.nothing"),
      );
    } else {
      toast.success(t("meetings.vouch.stopped"));
    }
    onClose();
  };

  if (!state) return null;
  const vouched = state.vouch.one_on_one !== null || state.vouch.trusted;
  const pill =
    "rounded-full border border-stone/30 px-2.5 py-0.5 hover:border-accent hover:text-accent cursor-pointer disabled:opacity-50 disabled:cursor-default";

  return (
    <div className="mx-1 rounded-lg border border-stone/20 bg-surface px-3 py-2 text-sm space-y-2">
      <div className="text-text/60">{t("meetings.vouch.explain")}</div>
      {vouched && (
        <div className="text-text/70">
          {state.vouch.one_on_one
            ? t("meetings.vouch.isOneOnOne", { name: state.vouch.one_on_one })
            : t("meetings.vouch.isTrusted")}
          {Object.keys(state.learned).length > 0 &&
            ` · ${learnedList(state.learned)}`}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={name}
          placeholder={t("meetings.speaker.namePlaceholder")}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && name.trim()) {
              save({ one_on_one: name.trim(), trusted: false });
            }
            if (e.key === "Escape") onClose();
          }}
          className="w-40 rounded border border-stone/30 bg-background px-1.5 py-0.5 outline-none focus:border-accent/60"
        />
        <button
          disabled={working || !name.trim()}
          onClick={() => save({ one_on_one: name.trim(), trusted: false })}
          className={pill}
          title={t("meetings.vouch.oneOnOneHint")}
        >
          {t("meetings.vouch.oneOnOne")}
        </button>
        <button
          disabled={working}
          onClick={() => save({ one_on_one: null, trusted: true })}
          className={pill}
          title={t("meetings.vouch.trustHint")}
        >
          {t("meetings.vouch.trust")}
        </button>
        {vouched && (
          <button
            disabled={working}
            onClick={() => save({ one_on_one: null, trusted: false })}
            className={pill}
          >
            {t("meetings.vouch.stop")}
          </button>
        )}
        <button
          onClick={onClose}
          className="text-text/55 hover:text-text cursor-pointer"
        >
          {t("meetings.transcript.cancel")}
        </button>
        {working && (
          <Loader2 className="w-3.5 h-3.5 animate-spin text-text/50" />
        )}
      </div>
    </div>
  );
};

const TranscriptSection: React.FC<{
  meeting: MeetingInfo;
  progress: TranscribeProgress | null;
  player: Player;
}> = ({ meeting, progress, player }) => {
  const { t } = useTranslation();
  const [transcript, setTranscript] = useState<MeetingTranscript | null>(null);
  const [showOriginal, setShowOriginal] = useState(false);
  const [confirmAgain, setConfirmAgain] = useState(false);
  const [vouching, setVouching] = useState(false);
  const closeVouch = useCallback(() => setVouching(false), []);
  const who = useWho();
  const peopleOf = usePeople();
  const [version, setVersion] = useState(0);
  const reload = useCallback(() => setVersion((v) => v + 1), []);
  const done = progress?.done;
  const stage = progress?.stage;

  const [doubts, setDoubts] = useState<Partial<Record<string, string>>>({});

  useEffect(() => {
    let cancelled = false;
    commands.getMeetingTranscript(meeting.id).then((result) => {
      if (!cancelled && result.status === "ok") setTranscript(result.data);
    });
    commands.speakerDoubts(meeting.id).then((result) => {
      if (!cancelled && result.status === "ok") setDoubts(result.data);
    });
    return () => {
      cancelled = true;
    };
  }, [meeting.id, meeting.transcript, meeting.summary, done, stage, version]);

  const retry = async () => {
    const result = await commands.transcribeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const again = async () => {
    setConfirmAgain(false);
    const result = await commands.retranscribeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const learnVoice = async () => {
    const result = await commands.learnMyVoiceFrom(meeting.id);
    if (result.status === "error") toast.error(result.error);
    else
      toast.success(
        t("meetings.transcript.learnedVoice", { mic: result.data }),
      );
  };

  const copy = async () => {
    const lines = (transcript?.paragraphs ?? []).map((p) => {
      const label = who(meeting, p);
      const text = showOriginal && p.raw ? p.raw : p.text;
      return `[${clock(p.start_ms)}] ${label ? `${label}: ` : ""}${text}`;
    });
    await navigator.clipboard.writeText(lines.join("\n\n"));
    toast.success(t("meetings.transcript.copied"));
  };

  const status = meeting.transcript;
  const paragraphs = transcript?.paragraphs ?? [];
  const people = peopleOf(meeting, paragraphs);
  const nowMs = player.time * 1000;
  const activeIndex = player.playing
    ? paragraphs.findIndex((p) => nowMs >= p.start_ms && nowMs < p.end_ms)
    : -1;

  let banner: React.ReactNode = null;
  if (status === "queued") {
    banner = (
      <span className="inline-flex items-center gap-2">
        <Clock className="w-3.5 h-3.5" />
        {t("meetings.transcript.queued")}
      </span>
    );
  } else if (status === "transcribing") {
    banner = (
      <span className="inline-flex items-center gap-2">
        <Loader2 className="w-3.5 h-3.5 animate-spin" />
        {progress?.stage === "identifying"
          ? progress.total > 0
            ? t("meetings.transcript.downloadingModel", {
                percent: Math.round((progress.done / progress.total) * 100),
              })
            : t("meetings.transcript.identifying")
          : progress && progress.total > 0
            ? t("meetings.transcript.transcribing", {
                done: progress.done,
                total: progress.total,
              })
            : t("meetings.transcript.preparing")}
      </span>
    );
  } else if (status === "failed") {
    banner = (
      <span className="inline-flex flex-wrap items-center gap-2 text-warning">
        <AlertTriangle className="w-3.5 h-3.5" />
        {t("meetings.transcript.failed", {
          error: meeting.transcript_error ?? "",
        })}
        <button
          onClick={retry}
          className="inline-flex items-center gap-1 rounded-full border border-stone/30 px-2 py-0.5 text-text hover:border-stone/50 cursor-pointer"
        >
          <RotateCcw className="w-3 h-3" />
          {t("meetings.transcript.retry")}
        </button>
      </span>
    );
  } else if (status === null && meeting.tracks.length > 0) {
    banner = (
      <span className="inline-flex items-center gap-2">
        {t("meetings.transcript.none")}
        <button
          onClick={retry}
          className="rounded-full bg-text text-background px-3 py-0.5 hover:opacity-90 cursor-pointer"
        >
          {t("meetings.transcript.start")}
        </button>
      </span>
    );
  }

  const progressPercent =
    status === "transcribing" && progress && progress.total > 0
      ? (progress.done / progress.total) * 100
      : null;
  const hasOriginal = paragraphs.some((p) => p.raw);
  const busy = progress !== null;
  const linkButton =
    "inline-flex items-center gap-1 normal-case tracking-normal font-normal text-text/55 hover:text-accent cursor-pointer";

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-3 px-1 text-sm font-medium text-text/70">
        <span className="flex-1">{t("meetings.transcript.title")}</span>
        {hasOriginal && (
          <button
            onClick={() => setShowOriginal(!showOriginal)}
            className={linkButton}
          >
            {showOriginal
              ? t("meetings.transcript.showCleaned")
              : t("meetings.transcript.showOriginal")}
          </button>
        )}
        {paragraphs.length > 0 && (
          <button onClick={copy} className={linkButton}>
            <Copy className="w-3 h-3" />
            {t("meetings.transcript.copy")}
          </button>
        )}
        {status === "done" && !busy && meeting.mode === "call" && (
          <button
            onClick={learnVoice}
            className={linkButton}
            title={t("meetings.transcript.learnVoiceHint")}
          >
            {t("meetings.transcript.learnVoice")}
          </button>
        )}
        {status === "done" && !busy && meeting.mode === "call" && (
          <button
            onClick={() => setVouching(!vouching)}
            className={linkButton}
            title={t("meetings.vouch.openHint")}
          >
            {t("meetings.vouch.open")}
          </button>
        )}
        {status === "done" &&
          !busy &&
          (confirmAgain ? (
            <span className="inline-flex items-center gap-2 normal-case tracking-normal font-normal">
              {t("meetings.transcript.againConfirm")}
              <button
                onClick={again}
                className="rounded-full bg-text text-background px-2 py-0.5 cursor-pointer"
              >
                {t("meetings.transcript.againYes")}
              </button>
              <button
                onClick={() => setConfirmAgain(false)}
                className="text-text/55 hover:text-text cursor-pointer"
              >
                {t("meetings.transcript.cancel")}
              </button>
            </span>
          ) : (
            <button
              onClick={() => setConfirmAgain(true)}
              className={linkButton}
            >
              <RotateCcw className="w-3 h-3" />
              {t("meetings.transcript.again")}
            </button>
          ))}
      </div>
      {vouching && status === "done" && !busy && (
        <VouchVoices meeting={meeting} onClose={closeVouch} />
      )}
      {banner && <div className="px-1 text-sm text-text/60">{banner}</div>}
      {progressPercent !== null && (
        <div className="mx-1 h-1 rounded-full bg-stone/15 overflow-hidden">
          <div
            className="h-full bg-accent transition-[width] duration-500"
            style={{ width: `${progressPercent}%` }}
          />
        </div>
      )}
      {paragraphs.length > 0 ? (
        <div className="rounded-xl border border-stone/20 bg-surface p-2">
          {paragraphs.map((p, i) => {
            const prevStart = i > 0 ? paragraphs[i - 1].start_ms : -1;
            const resumed = (meeting.resumed_at_ms ?? []).some(
              (r) => r > prevStart && r <= p.start_ms,
            );
            return (
              <React.Fragment key={`${p.source}-${p.start_ms}`}>
                {resumed && (
                  <div className="flex items-center gap-2 px-2 py-1 text-xs text-text/45">
                    <span className="h-px flex-1 bg-stone/20" />
                    {t("meetings.transcript.resumed")}
                    <span className="h-px flex-1 bg-stone/20" />
                  </div>
                )}
                <TranscriptParagraph
                  p={p}
                  meeting={meeting}
                  showOriginal={showOriginal}
                  active={i === activeIndex}
                  editable={status === "done" && !showOriginal}
                  people={people}
                  doubt={doubts[`${p.source}-${p.start_ms}`]}
                  onPlay={() => player.playFrom(p.start_ms / 1000)}
                  onChanged={reload}
                />
              </React.Fragment>
            );
          })}
        </div>
      ) : (
        status === "done" && (
          <p className="px-1 text-sm text-text/50">
            {t("meetings.transcript.empty")}
          </p>
        )
      )}
    </div>
  );
};

const SummaryList: React.FC<{ title: string; items: React.ReactNode[] }> = ({
  title,
  items,
}) =>
  items.length === 0 ? null : (
    <div className="space-y-1.5">
      <h3 className="text-sm font-medium">{title}</h3>
      <ul className="space-y-1 ps-5 list-disc marker:text-text/35">
        {items.map((item, i) => (
          <li key={i} className="text-sm leading-relaxed text-text/85">
            {item}
          </li>
        ))}
      </ul>
    </div>
  );

/** The written notes: overview, key points, decisions and action items. */
const SummarySection: React.FC<{
  meeting: MeetingInfo;
  progress: TranscribeProgress | null;
  player: Player;
}> = ({ meeting, progress, player }) => {
  const { t } = useTranslation();
  const [summary, setSummary] = useState<Summary | null>(null);

  useEffect(() => {
    let cancelled = false;
    commands.getMeetingSummary(meeting.id).then((result) => {
      if (!cancelled && result.status === "ok") setSummary(result.data);
    });
    return () => {
      cancelled = true;
    };
  }, [meeting.id, meeting.summary]);

  const generate = async () => {
    const result = await commands.summarizeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const copyMarkdown = async () => {
    const result = await commands.meetingMarkdown(meeting.id);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    await navigator.clipboard.writeText(result.data);
    toast.success(t("meetings.summary.copied"));
  };

  const status = meeting.summary;
  const transcribed = meeting.transcript === "done";
  const working = status === "queued" || status === "running";

  let banner: React.ReactNode = null;
  if (status === "queued") {
    banner = (
      <span className="inline-flex items-center gap-2">
        <Clock className="w-3.5 h-3.5" />
        {t("meetings.summary.queued")}
      </span>
    );
  } else if (status === "running") {
    banner = (
      <span className="inline-flex items-center gap-2">
        <Loader2 className="w-3.5 h-3.5 animate-spin" />
        {progress?.stage === "cleaning_up" && progress.total > 0
          ? t("meetings.summary.cleaningUp", {
              done: progress.done,
              total: progress.total,
            })
          : progress?.stage === "summarizing" && progress.total > 1
            ? t("meetings.summary.summarizingPart", {
                done: progress.done,
                total: progress.total,
              })
            : t("meetings.summary.summarizing")}
      </span>
    );
  } else if (status === "failed") {
    banner = (
      <span className="inline-flex flex-wrap items-center gap-2 text-warning">
        <AlertTriangle className="w-3.5 h-3.5" />
        {t("meetings.summary.failed", { error: meeting.summary_error ?? "" })}
        <button
          onClick={generate}
          className="inline-flex items-center gap-1 rounded-full border border-stone/30 px-2 py-0.5 text-text hover:border-stone/50 cursor-pointer"
        >
          <RotateCcw className="w-3 h-3" />
          {t("meetings.summary.retry")}
        </button>
      </span>
    );
  } else if (!summary && transcribed) {
    banner = (
      <span className="inline-flex items-center gap-2">
        {t("meetings.summary.none")}
        <button
          onClick={generate}
          className="inline-flex items-center gap-1.5 rounded-full bg-text text-background px-3 py-0.5 hover:opacity-90 cursor-pointer"
        >
          <Sparkles className="w-3 h-3" />
          {t("meetings.summary.generate")}
        </button>
      </span>
    );
  } else if (!summary && meeting.tracks.length > 0) {
    banner = t("meetings.summary.afterTranscript");
  }

  const linkButton =
    "inline-flex items-center gap-1 normal-case tracking-normal font-normal text-text/55 hover:text-accent cursor-pointer";
  const jump = (ms: number | null) =>
    ms === null ? null : (
      <button
        onClick={() => player.playFrom(ms / 1000)}
        title={t("meetings.transcript.playFrom", { time: clock(ms) })}
        className="ms-1.5 text-xs tabular-nums text-text/45 hover:text-accent cursor-pointer"
      >
        {clock(ms)}
      </button>
    );

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-3 px-1 text-sm font-medium text-text/70">
        <span className="flex-1">{t("meetings.summary.title")}</span>
        {summary && (
          <button onClick={copyMarkdown} className={linkButton}>
            <Copy className="w-3 h-3" />
            {t("meetings.summary.copyMarkdown")}
          </button>
        )}
        {summary && transcribed && !working && (
          <button onClick={generate} className={linkButton}>
            <Sparkles className="w-3 h-3" />
            {t("meetings.summary.regenerate")}
          </button>
        )}
      </div>
      {banner && <div className="px-1 text-sm text-text/60">{banner}</div>}
      {summary && (
        <div
          className={`rounded-xl border border-stone/20 bg-accent/5 p-5 space-y-4 ${
            working ? "opacity-60" : ""
          }`}
        >
          {summary.overview && (
            <p className="text-sm leading-relaxed">{summary.overview}</p>
          )}
          <SummaryList
            title={t("meetings.summary.keyPoints")}
            items={summary.key_points}
          />
          <SummaryList
            title={t("meetings.summary.decisions")}
            items={summary.decisions}
          />
          <SummaryList
            title={t("meetings.summary.actionItems")}
            items={summary.action_items.map((a) => (
              <>
                {a.owner && (
                  <span className="font-medium me-1">{a.owner}:</span>
                )}
                {a.task}
                {jump(a.at_ms)}
              </>
            ))}
          />
          <p className="text-xs text-text/40">
            {t("meetings.summary.by", { model: summary.model })}
          </p>
        </div>
      )}
    </div>
  );
};

/** Questions about a finished meeting, and a follow-up email. */
const AskSection: React.FC<{ meeting: MeetingInfo }> = ({ meeting }) => {
  const { t } = useTranslation();
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);

  const ask = async () => {
    const q = question.trim();
    if (!q || asking) return;
    setAsking(true);
    const result = await commands.askMeeting(meeting.id, q);
    setAsking(false);
    if (result.status === "error") {
      toast.error(result.error);
      return;
    }
    setQuestion("");
    if (result.data.summary_updated) {
      setAnswer(null);
      toast.success(t("meetings.ask.notesUpdated"));
    } else {
      setAnswer(result.data.answer);
    }
  };

  const followUp = async () => {
    setAsking(true);
    const result = await commands.draftFollowUpEmail(meeting.id);
    setAsking(false);
    if (result.status === "error") toast.error(result.error);
    else setAnswer(result.data);
  };

  const copy = async () => {
    if (!answer) return;
    await navigator.clipboard.writeText(answer);
    toast.success(t("meetings.ask.copied"));
  };

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-3 px-1 text-sm font-medium text-text/70">
        <span className="flex-1">{t("meetings.ask.title")}</span>
        <button
          onClick={followUp}
          disabled={asking}
          className="inline-flex items-center gap-1 font-normal text-text/55 hover:text-accent cursor-pointer disabled:opacity-50"
        >
          <Send className="w-3 h-3" />
          {t("meetings.ask.followUp")}
        </button>
      </div>
      <div className="flex items-center gap-2 rounded-xl border border-stone/20 bg-surface px-3 py-2">
        <MessageCircleQuestion className="w-4 h-4 text-text/40 shrink-0" />
        <input
          value={question}
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") ask();
          }}
          placeholder={t("meetings.ask.placeholder")}
          className="flex-1 bg-transparent text-sm outline-none placeholder:text-text/35"
        />
        {asking ? (
          <Loader2 className="w-4 h-4 animate-spin text-text/50" />
        ) : (
          <button
            onClick={ask}
            disabled={!question.trim()}
            className="rounded-full bg-text text-background px-3 py-0.5 text-sm hover:opacity-90 cursor-pointer disabled:opacity-40"
          >
            {t("meetings.ask.submit")}
          </button>
        )}
      </div>
      {answer && (
        <div className="rounded-xl border border-stone/20 bg-accent/5 p-4 space-y-2">
          <p className="text-sm leading-relaxed whitespace-pre-wrap">
            {answer}
          </p>
          <button
            onClick={copy}
            className="inline-flex items-center gap-1 text-xs text-text/55 hover:text-accent cursor-pointer"
          >
            <Copy className="w-3 h-3" />
            {t("meetings.ask.copy")}
          </button>
        </div>
      )}
    </div>
  );
};

/** Resume, show in Finder, move to the Trash. */
const MeetingActions: React.FC<{
  meeting: MeetingInfo;
  canResume: boolean;
  onGone: () => void;
}> = ({ meeting, canResume, onGone }) => {
  const { t } = useTranslation();
  const [confirmDelete, setConfirmDelete] = useState(false);
  const link =
    "inline-flex items-center gap-1.5 text-xs text-text/60 hover:text-accent cursor-pointer";

  const resume = async () => {
    const result = await commands.resumeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const remove = async () => {
    setConfirmDelete(false);
    const result = await commands.deleteMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
    else onGone();
  };

  return (
    <div className="flex flex-wrap items-center gap-4 px-1">
      <button
        onClick={() => commands.openMeetingFolder(meeting.id)}
        className={link}
      >
        <FolderOpen className="w-3.5 h-3.5" />
        {t("meetings.showInFinder")}
      </button>
      {canResume && meeting.tracks.length > 0 && (
        <button onClick={resume} className={link}>
          <Mic className="w-3.5 h-3.5" />
          {t("meetings.actions.resume")}
        </button>
      )}
      {confirmDelete ? (
        <span className="inline-flex items-center gap-2 text-xs">
          {t("meetings.actions.deleteConfirm")}
          <button
            onClick={remove}
            className="rounded-full bg-error text-white px-2 py-0.5 cursor-pointer"
          >
            {t("meetings.actions.deleteYes")}
          </button>
          <button
            onClick={() => setConfirmDelete(false)}
            className="text-text/55 hover:text-text cursor-pointer"
          >
            {t("meetings.actions.cancel")}
          </button>
        </span>
      ) : (
        <button onClick={() => setConfirmDelete(true)} className={link}>
          <Trash2 className="w-3.5 h-3.5" />
          {t("meetings.actions.delete")}
        </button>
      )}
    </div>
  );
};

/** One meeting, like a Granola note: title, your notes, the transcript. */
const MeetingDetail: React.FC<{
  meeting: MeetingInfo;
  progress: TranscribeProgress | null;
  canResume: boolean;
  onBack: () => void;
}> = ({ meeting, progress, canResume, onBack }) => {
  const { t, i18n } = useTranslation();
  const player = useMeetingPlayer(meeting);
  const started = new Date(meeting.started_at);
  const date = started.toLocaleDateString(i18n.language, {
    weekday: "short",
    month: "short",
    day: "numeric",
  });
  const time = started.toLocaleTimeString(i18n.language, {
    hour: "numeric",
    minute: "2-digit",
  });
  const recorded = Math.max(0, ...meeting.tracks.map((tr) => tr.seconds));
  const duration =
    recorded > 0
      ? recorded * 1000
      : meeting.ended_at
        ? meeting.ended_at - meeting.started_at
        : null;

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <button
        onClick={onBack}
        className="inline-flex items-center gap-1.5 px-1 text-sm text-text/60 hover:text-text cursor-pointer"
      >
        <ArrowLeft className="w-4 h-4" />
        {t("meetings.back")}
      </button>

      <div className="space-y-2 px-1">
        <TitleEditor
          key={`${meeting.id}-${meeting.title}`}
          meeting={meeting}
          className="text-3xl"
        />
        <div className="flex flex-wrap gap-1.5">
          <Chip icon={<Calendar className="w-3 h-3" />}>
            {date} · {time}
          </Chip>
          {duration !== null && (
            <Chip icon={<Clock className="w-3 h-3" />}>{clock(duration)}</Chip>
          )}
          <Chip icon={modeIcon(meeting.mode, "w-3 h-3")}>
            {meeting.mode === "call"
              ? t("meetings.mode.call")
              : t("meetings.mode.inPerson")}
          </Chip>
          {meeting.status === "interrupted" && (
            <Chip tone="warning">{t("meetings.interrupted")}</Chip>
          )}
          {meeting.status === "paused" && <Chip>{t("meetings.paused")}</Chip>}
          {meeting.mode === "call" &&
            !meeting.tracks.some((tr) => tr.file === "system.wav") && (
              <Chip tone="warning">{t("meetings.noSystemAudio")}</Chip>
            )}
        </div>
      </div>

      <SummarySection
        meeting={meeting}
        progress={progress?.id === meeting.id ? progress : null}
        player={player}
      />

      {meeting.transcript === "done" && <AskSection meeting={meeting} />}

      <NotesEditor id={meeting.id} />

      {meeting.tracks.length > 0 && (
        <div className="sticky top-0 z-10">
          <PlayerBar player={player} />
        </div>
      )}

      <TranscriptSection
        meeting={meeting}
        progress={progress?.id === meeting.id ? progress : null}
        player={player}
      />

      <MeetingActions meeting={meeting} canResume={canResume} onGone={onBack} />
    </div>
  );
};

const RecorderCard: React.FC<{
  live: MeetingState;
  onChanged: () => void;
}> = ({ live, onChanged }) => {
  const { t, i18n } = useTranslation();
  const { getSetting } = useSettings();
  // "auto" works out whether it's a call (from the call app, its windows
  // and what comes through the speakers).
  const [mode, setMode] = useState<MeetingMode | "auto">(
    (getSetting("meeting_detect_mode") ?? true)
      ? "auto"
      : (getSetting("meeting_mode") ?? "call"),
  );
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(Date.now());
  const [receivedAt, setReceivedAt] = useState(Date.now());
  const [seen, setSeen] = useState(live);
  const recording = live.recording;
  // The recording's length is known when the state arrives; count on from there.
  if (seen !== live) {
    setSeen(live);
    setReceivedAt(Date.now());
  }

  useEffect(() => {
    if (!recording) return;
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, [recording]);

  const start = async () => {
    setBusy(true);
    const result = await commands.startMeeting(mode === "auto" ? null : mode);
    setBusy(false);
    if (result.status === "error") {
      toast.error(t("meetings.errors.start", { error: result.error }));
    }
    onChanged();
  };

  const stop = async () => {
    setBusy(true);
    const result = await commands.stopMeeting();
    setBusy(false);
    if (result.status === "error") {
      toast.error(t("meetings.errors.stop", { error: result.error }));
    }
    onChanged();
  };

  if (recording) {
    const startedAt = new Date(recording.started_at).toLocaleTimeString(
      i18n.language,
      { hour: "numeric", minute: "2-digit" },
    );
    return (
      <div className="rounded-xl border border-stone/20 bg-surface p-5 space-y-4">
        <div className="space-y-2">
          <TitleEditor
            key={`${recording.id}-${recording.title}`}
            meeting={recording}
            className="text-2xl"
          />
          <div className="flex flex-wrap gap-1.5">
            <Chip icon={<Clock className="w-3 h-3" />}>
              {t("meetings.startedAt", { time: startedAt })}
            </Chip>
            <Chip icon={<Mic className="w-3 h-3" />}>{recording.mic}</Chip>
            {recording.mode === "call" &&
              (recording.system_error ? (
                <Chip
                  tone="warning"
                  icon={<AlertTriangle className="w-3 h-3" />}
                >
                  {t("meetings.noSystemAudio")}
                </Chip>
              ) : (
                <Chip icon={<Monitor className="w-3 h-3" />}>
                  {t("meetings.systemAudio")}
                </Chip>
              ))}
          </div>
        </div>
        {recording.system_error && (
          <p className="text-sm text-text/60">{recording.system_error}</p>
        )}
        <NotesEditor id={recording.id} minRows={4} />
        {live.live && <LiveHelp />}
        <p className="px-1 text-sm text-text/45">
          {t("meetings.transcript.afterRecording")}
        </p>
        <div className="flex justify-center">
          <div className="inline-flex items-center gap-3 rounded-full bg-text text-background pl-4 pr-1.5 py-1.5 shadow-md">
            <span className="text-highlight">
              <LiveBars />
            </span>
            <span className="text-sm tabular-nums">
              {t("meetings.recordingFor", {
                time: clock(live.elapsed_ms + (now - receivedAt)),
              })}
            </span>
            <button
              onClick={stop}
              disabled={busy}
              className="inline-flex items-center gap-1.5 rounded-full bg-background/15 hover:bg-background/25 px-3 py-1 text-sm cursor-pointer disabled:opacity-50"
            >
              <Square className="w-3 h-3 fill-current" />
              {t("meetings.stop")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  const modes: {
    id: MeetingMode | "auto";
    label: string;
    hint: string;
  }[] = [
    {
      id: "auto",
      label: t("meetings.mode.auto"),
      hint: t("meetings.mode.autoHint"),
    },
    {
      id: "call",
      label: t("meetings.mode.call"),
      hint: t("meetings.mode.callHint"),
    },
    {
      id: "in_person",
      label: t("meetings.mode.inPerson"),
      hint: t("meetings.mode.inPersonHint"),
    },
  ];
  return (
    <div className="rounded-xl border border-stone/20 bg-surface p-5 space-y-4">
      <div className="grid grid-cols-3 gap-2">
        {modes.map((m) => (
          <button
            key={m.id}
            onClick={() => setMode(m.id)}
            className={`text-start rounded-xl border p-3 transition-colors cursor-pointer ${
              mode === m.id
                ? "border-accent bg-accent/10"
                : "border-stone/20 hover:border-stone/30"
            }`}
          >
            <div className="flex items-center gap-2 text-sm font-medium">
              {m.id === "auto" ? (
                <Sparkles className="w-4 h-4" />
              ) : (
                modeIcon(m.id, "w-4 h-4")
              )}
              {m.label}
            </div>
            <p className="mt-1 text-sm text-text/60">{m.hint}</p>
          </button>
        ))}
      </div>
      <div className="flex justify-center">
        <button
          onClick={start}
          disabled={busy}
          className="inline-flex items-center gap-2 rounded-full bg-text text-background px-5 py-2 text-sm shadow-md hover:opacity-90 cursor-pointer disabled:opacity-50"
        >
          <span className="w-2.5 h-2.5 rounded-full bg-error" />
          {t("meetings.start")}
        </button>
      </div>
    </div>
  );
};

const MeetingRow: React.FC<{
  meeting: MeetingInfo;
  progress: TranscribeProgress | null;
  onOpen: () => void;
}> = ({ meeting, progress, onOpen }) => {
  const { t, i18n } = useTranslation();
  const titleOf = useMeetingTitle();
  const duration = meeting.ended_at
    ? meeting.ended_at - meeting.started_at
    : null;
  const time = new Date(meeting.started_at).toLocaleTimeString(i18n.language, {
    hour: "numeric",
    minute: "2-digit",
  });
  const percent =
    progress && progress.id === meeting.id && progress.total > 0
      ? Math.round((progress.done / progress.total) * 100)
      : 0;

  return (
    <button
      onClick={onOpen}
      className="w-full py-3 flex items-center gap-3 text-start cursor-pointer group"
    >
      <span className="w-8 h-8 shrink-0 rounded-lg bg-stone/10 flex items-center justify-center text-text/70">
        {modeIcon(meeting.mode, "w-4 h-4")}
      </span>
      <span className="flex-1 min-w-0">
        <span className="block text-sm font-medium truncate group-hover:text-accent">
          {titleOf(meeting)}
        </span>
        <span className="flex flex-wrap gap-1.5 mt-1">
          <Chip>{time}</Chip>
          {duration !== null && <Chip>{clock(duration)}</Chip>}
          {meeting.status === "interrupted" && (
            <Chip tone="warning">{t("meetings.interrupted")}</Chip>
          )}
          {meeting.status === "paused" && <Chip>{t("meetings.paused")}</Chip>}
          {meeting.transcript === "queued" && (
            <Chip>{t("meetings.status.queued")}</Chip>
          )}
          {meeting.transcript === "transcribing" && (
            <Chip
              tone="accent"
              icon={<Loader2 className="w-3 h-3 animate-spin" />}
            >
              {progress?.id === meeting.id && progress.stage === "identifying"
                ? t("meetings.status.identifying")
                : t("meetings.status.transcribing", { percent })}
            </Chip>
          )}
          {meeting.transcript === "failed" && (
            <Chip tone="warning">{t("meetings.status.failed")}</Chip>
          )}
          {meeting.summary === "running" && (
            <Chip
              tone="accent"
              icon={<Loader2 className="w-3 h-3 animate-spin" />}
            >
              {progress?.id === meeting.id && progress.stage === "cleaning_up"
                ? t("meetings.status.cleaningUp", { percent })
                : t("meetings.status.summarizing")}
            </Chip>
          )}
          {meeting.summary === "failed" && (
            <Chip tone="warning">{t("meetings.status.summaryFailed")}</Chip>
          )}
          {meeting.mode === "call" &&
            !meeting.tracks.some((tr) => tr.file === "system.wav") &&
            meeting.status !== "recording" && (
              <Chip tone="warning">{t("meetings.noSystemAudio")}</Chip>
            )}
        </span>
      </span>
      <ChevronRight className="w-4 h-4 text-text/40" />
    </button>
  );
};

/** "Today", "Yesterday" or the date, for grouping the list. */
const dayLabel = (
  ms: number,
  locale: string,
  t: (k: string) => string,
): string => {
  const day = new Date(ms);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  const same = (a: Date, b: Date) => a.toDateString() === b.toDateString();
  if (same(day, today)) return t("meetings.today");
  if (same(day, yesterday)) return t("meetings.yesterday");
  return day.toLocaleDateString(locale, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
};

export const MeetingsPage: React.FC = () => {
  const { t, i18n } = useTranslation();
  const [live, setLive] = useState<MeetingState>({
    recording: null,
    paused: null,
    elapsed_ms: 0,
    transcribing: null,
    live: false,
  });
  const [meetings, setMeetings] = useState<MeetingInfo[]>([]);
  const [openId, setOpenId] = useState<string | null>(null);

  const refreshList = useCallback(async () => {
    setMeetings(await commands.listMeetings());
  }, []);

  const refresh = useCallback(async () => {
    const [state, list] = await Promise.all([
      commands.getMeetingState(),
      commands.listMeetings(),
    ]);
    setLive(state);
    setMeetings(list);
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = [
      listen<MeetingState>("meeting-state", (e) => {
        setLive(e.payload);
        // Progress events are frequent; the list only changes on start/stop.
        if (!e.payload.transcribing) refreshList();
      }),
      listen("meetings-changed", () => refreshList()),
      listen<string>("meeting-error", (e) =>
        toast.error(t("meetings.errors.start", { error: e.payload })),
      ),
    ];
    return () => {
      unlisten.forEach((p) => p.then((f) => f()));
    };
  }, [refresh, refreshList, t]);

  const past = useMemo(
    () => meetings.filter((m) => m.id !== live.recording?.id),
    [meetings, live.recording],
  );
  const groups = useMemo(() => {
    const out: { label: string; items: MeetingInfo[] }[] = [];
    for (const m of past) {
      const label = dayLabel(m.started_at, i18n.language, t);
      const last = out[out.length - 1];
      if (last?.label === label) last.items.push(m);
      else out.push({ label, items: [m] });
    }
    return out;
  }, [past, i18n.language, t]);

  const opened = past.find((m) => m.id === openId);
  if (opened) {
    return (
      <MeetingDetail
        key={opened.id}
        meeting={opened}
        progress={live.transcribing}
        canResume={live.recording === null}
        onBack={() => setOpenId(null)}
      />
    );
  }

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("meetings.heading")}
        description={t("meetings.subheading")}
        actions={
          <button
            onClick={() => openSection("meetingSettings")}
            className="inline-flex items-center gap-1.5 rounded-full border border-stone/25 px-3 py-1 text-sm text-text/70 hover:text-text hover:border-stone/40 cursor-pointer"
          >
            <Settings2 className="w-3.5 h-3.5" />
            {t("meetings.settings.open")}
          </button>
        }
      />
      <p className="rounded-xl bg-highlight/15 px-4 py-3 text-sm leading-snug text-text/80">
        {t("meetings.wip")}
      </p>

      <RecorderCard live={live} onChanged={refresh} />

      {groups.length === 0 ? (
        <p className="px-1 py-6 text-center font-display text-lg text-text/50">
          {t("meetings.empty")}
        </p>
      ) : (
        groups.map((group) => (
          <div key={group.label} className="space-y-1">
            <div className="flex items-center gap-1.5 px-1 text-sm font-medium text-text/70">
              <Calendar className="w-3.5 h-3.5" />
              {group.label}
            </div>
            <div className="rounded-xl border border-stone/20 bg-surface px-4 divide-y divide-stone/15">
              {group.items.map((m) => (
                <MeetingRow
                  key={m.id}
                  meeting={m}
                  progress={live.transcribing}
                  onOpen={() => setOpenId(m.id)}
                />
              ))}
            </div>
          </div>
        ))
      )}
    </div>
  );
};

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
  Mic,
  Monitor,
  Pause,
  Play,
  RotateCcw,
  Sparkles,
  Square,
  Users,
  Video,
} from "lucide-react";
import {
  commands,
  type MeetingInfo,
  type MeetingMode,
  type MeetingState,
  type MeetingTranscript,
  type Paragraph,
  type Summary,
  type TranscribeProgress,
} from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { PageHeader } from "../ui/PageHeader";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ShortcutInput } from "../settings/ShortcutInput";
import { MeetingSettings } from "./MeetingSettings";

/** `m:ss`, or `h:mm:ss` from an hour. */
const clock = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
};

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

/** Animated level bars for the recording pill. */
const LiveBars: React.FC = () => (
  <span className="flex items-end gap-[3px] h-4" aria-hidden>
    {[0, 1, 2, 3].map((i) => (
      <span
        key={i}
        className="meeting-bar w-[3px] rounded-full bg-current"
        style={{ animationDelay: `${i * 0.15}s` }}
      />
    ))}
  </span>
);

const modeIcon = (mode: MeetingMode, size = "w-3.5 h-3.5") =>
  mode === "call" ? <Video className={size} /> : <Users className={size} />;

const useMeetingTitle = () => {
  const { t } = useTranslation();
  return (m: MeetingInfo) =>
    m.title ??
    (m.mode === "call"
      ? t("meetings.title.call")
      : t("meetings.title.inPerson"));
};

/** The meeting's title, edited in place like a document heading. Keyed by
 * id and title, so it resets when either changes. */
const TitleEditor: React.FC<{
  meeting: MeetingInfo;
  className?: string;
}> = ({ meeting, className = "" }) => {
  const { t } = useTranslation();
  const titleOf = useMeetingTitle();
  const [value, setValue] = useState(titleOf(meeting));

  const save = async () => {
    if (value.trim() === titleOf(meeting)) return;
    const result = await commands.renameMeeting(meeting.id, value);
    if (result.status === "error") toast.error(result.error);
  };
  return (
    <input
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onBlur={save}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
      }}
      placeholder={t("meetings.titlePlaceholder")}
      className={`w-full bg-transparent font-serif outline-none placeholder:text-text/30 ${className}`}
    />
  );
};

type SaveState = "idle" | "saving" | "saved";

/** The user's own notes, saved as they type. They go into the summary. */
const NotesEditor: React.FC<{ id: string; minRows?: number }> = ({
  id,
  minRows = 5,
}) => {
  const { t } = useTranslation();
  const [notes, setNotes] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const pending = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();
  const area = useRef<HTMLTextAreaElement>(null);

  const flush = useCallback(async () => {
    clearTimeout(timer.current);
    const text = pending.current;
    if (text === null) return;
    pending.current = null;
    setSaveState("saving");
    const result = await commands.saveMeetingNotes(id, text);
    if (result.status === "error") {
      toast.error(t("meetings.notes.saveFailed", { error: result.error }));
      setSaveState("idle");
    } else {
      setSaveState("saved");
    }
  }, [id, t]);

  useEffect(() => {
    let cancelled = false;
    commands.getMeetingNotes(id).then((result) => {
      if (!cancelled) setNotes(result.status === "ok" ? result.data : "");
    });
    return () => {
      cancelled = true;
      flush();
    };
  }, [id, flush]);

  // Grow with the text.
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [notes]);

  if (notes === null) return null;
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between px-1">
        <span className="text-sm font-medium text-text/70">
          {t("meetings.notes.title")}
        </span>
        <span className="text-xs text-text/40">
          {saveState === "saving"
            ? t("meetings.notes.saving")
            : saveState === "saved"
              ? t("meetings.notes.saved")
              : ""}
        </span>
      </div>
      <textarea
        ref={area}
        value={notes}
        rows={minRows}
        onChange={(e) => {
          setNotes(e.target.value);
          pending.current = e.target.value;
          setSaveState("idle");
          clearTimeout(timer.current);
          timer.current = setTimeout(flush, 600);
        }}
        onBlur={flush}
        placeholder={t("meetings.notes.placeholder")}
        className="w-full resize-none rounded-xl border border-stone/20 bg-background px-4 py-3 text-sm leading-relaxed outline-none focus:border-accent/60 placeholder:text-text/35"
      />
    </div>
  );
};

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

/** "Speaker 2", or the name the user gave that voice. */
const useSpeakerName = () => {
  const { t } = useTranslation();
  return (meeting: MeetingInfo, n: number) =>
    meeting.speakers?.[n] ?? t("meetings.speaker.numbered", { n: n + 1 });
};

/** A told-apart voice's name; click to rename it everywhere. */
const SpeakerName: React.FC<{ meeting: MeetingInfo; n: number }> = ({
  meeting,
  n,
}) => {
  const { t } = useTranslation();
  const nameOf = useSpeakerName();
  const [editing, setEditing] = useState<string | null>(null);
  const colors = [
    "text-accent",
    "text-accent",
    "text-success",
    "text-warning",
    "text-violet-500",
  ];
  const color = colors[n % colors.length];

  const save = async () => {
    const name = editing?.trim();
    setEditing(null);
    if (name === undefined || name === nameOf(meeting, n)) return;
    const result = await commands.renameMeetingSpeaker(meeting.id, n, name);
    if (result.status === "error") toast.error(result.error);
  };

  if (editing !== null) {
    return (
      <input
        autoFocus
        value={editing}
        onChange={(e) => setEditing(e.target.value)}
        onBlur={save}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") setEditing(null);
        }}
        className={`me-1.5 w-32 bg-transparent border-b border-accent/60 outline-none font-medium ${color}`}
      />
    );
  }
  return (
    <button
      onClick={() => setEditing(nameOf(meeting, n))}
      title={t("meetings.speaker.rename")}
      className={`me-1.5 font-medium cursor-text hover:underline decoration-dotted ${color}`}
    >
      {nameOf(meeting, n)}
    </button>
  );
};

const TranscriptParagraph: React.FC<{
  p: Paragraph;
  meeting: MeetingInfo;
  showOriginal: boolean;
  active: boolean;
  onPlay: () => void;
}> = ({ p, meeting, showOriginal, active, onPlay }) => {
  const { t } = useTranslation();
  const mode = meeting.mode;
  const speaker =
    mode === "call"
      ? p.source === "mic"
        ? t("meetings.speaker.me")
        : t("meetings.speaker.them")
      : null;
  const text = showOriginal && p.raw ? p.raw : p.text;
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
          <span
            className={`me-1.5 font-medium ${
              p.source === "mic" ? "text-text" : "text-accent"
            }`}
          >
            {speaker}
          </span>
        )}
        {mode === "in_person" && p.speaker !== null && (
          <SpeakerName meeting={meeting} n={p.speaker} />
        )}
        <span className="text-text/85">{text}</span>
      </p>
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
  const done = progress?.done;
  const stage = progress?.stage;

  useEffect(() => {
    let cancelled = false;
    commands.getMeetingTranscript(meeting.id).then((result) => {
      if (!cancelled && result.status === "ok") setTranscript(result.data);
    });
    return () => {
      cancelled = true;
    };
  }, [meeting.id, meeting.transcript, meeting.summary, done, stage]);

  const retry = async () => {
    const result = await commands.transcribeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const again = async () => {
    setConfirmAgain(false);
    const result = await commands.retranscribeMeeting(meeting.id);
    if (result.status === "error") toast.error(result.error);
  };

  const copy = async () => {
    const lines = (transcript?.paragraphs ?? []).map((p) => {
      const who =
        meeting.mode === "call"
          ? p.source === "mic"
            ? t("meetings.speaker.me")
            : t("meetings.speaker.them")
          : p.speaker !== null
            ? (meeting.speakers?.[p.speaker] ??
              t("meetings.speaker.numbered", { n: p.speaker + 1 }))
            : null;
      const text = showOriginal && p.raw ? p.raw : p.text;
      return `[${clock(p.start_ms)}] ${who ? `${who}: ` : ""}${text}`;
    });
    await navigator.clipboard.writeText(lines.join("\n\n"));
    toast.success(t("meetings.transcript.copied"));
  };

  const status = meeting.transcript;
  const paragraphs = transcript?.paragraphs ?? [];
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
          {paragraphs.map((p, i) => (
            <TranscriptParagraph
              key={`${p.source}-${p.start_ms}`}
              p={p}
              meeting={meeting}
              showOriginal={showOriginal}
              active={i === activeIndex}
              onPlay={() => player.playFrom(p.start_ms / 1000)}
            />
          ))}
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

/** One meeting, like a Granola note: title, your notes, the transcript. */
const MeetingDetail: React.FC<{
  meeting: MeetingInfo;
  progress: TranscribeProgress | null;
  onBack: () => void;
}> = ({ meeting, progress, onBack }) => {
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
  const duration = meeting.ended_at
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

      <button
        onClick={() => commands.openMeetingFolder(meeting.id)}
        className="inline-flex items-center gap-1.5 px-1 text-xs text-text/60 hover:text-accent cursor-pointer"
      >
        <FolderOpen className="w-3.5 h-3.5" />
        {t("meetings.showInFinder")}
      </button>
    </div>
  );
};

const RecorderCard: React.FC<{
  live: MeetingState;
  onChanged: () => void;
}> = ({ live, onChanged }) => {
  const { t, i18n } = useTranslation();
  const { getSetting } = useSettings();
  const [mode, setMode] = useState<MeetingMode>(
    getSetting("meeting_mode") ?? "call",
  );
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(Date.now());
  const recording = live.recording;

  useEffect(() => {
    if (!recording) return;
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, [recording]);

  const start = async () => {
    setBusy(true);
    const result = await commands.startMeeting(mode);
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
                time: clock(now - recording.started_at),
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

  const modes: { id: MeetingMode; label: string; hint: string }[] = [
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
      <div className="grid grid-cols-2 gap-2">
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
              {modeIcon(m.id, "w-4 h-4")}
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
    elapsed_ms: 0,
    transcribing: null,
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
        onBack={() => setOpenId(null)}
      />
    );
  }

  return (
    <div className="max-w-2xl w-full mx-auto space-y-6">
      <PageHeader
        title={t("meetings.heading")}
        description={t("meetings.subheading")}
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

      <SettingsGroup title={t("meetings.settings.title")}>
        <ShortcutInput shortcutId="meeting" grouped={true} />
        <MeetingSettings />
      </SettingsGroup>
    </div>
  );
};
